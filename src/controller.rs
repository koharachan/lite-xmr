use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::algorithms;
use crate::config::Config;
use crate::cpu::{APP_VERSION, CpuInfo, os_name};
use crate::daemon_rpc::DaemonRpcClient;
use crate::doh;
use crate::job;
use crate::miner::{self, MinedShare, Miner};
use crate::stats::MiningStats;
use crate::stratum::{StratumClient, StratumEvent};
use crate::taskbar::Taskbar;

pub struct Controller {
    running: Arc<AtomicBool>,
    taskbar: Taskbar,
}

impl Controller {
    pub fn new() -> Self {
        Controller {
            running: Arc::new(AtomicBool::new(false)),
            taskbar: Taskbar::new(),
        }
    }

    fn print_banner(&self) {
        info!(
            "* ABOUT        lite-xmr/{} {} x86-64",
            APP_VERSION,
            os_name()
        );
    }

    pub async fn run(&mut self, config: &Config) -> anyhow::Result<()> {
        self.running.store(true, Ordering::Release);
        self.taskbar.set_active(true);
        self.taskbar.set_enabled(true);

        self.print_banner();

        if config.daemon_rpc {
            return self.run_daemon_rpc(config).await;
        }

        let cpu_info = CpuInfo::detect();
        cpu_info.print_summary();
        let plan = cpu_info.build_thread_plan();

        let planned_pus = if config.use_e_cores {
            plan.preferred_with_e()
        } else {
            plan.preferred_p_only()
        };
        let threads = if config.threads == 0 {
            if planned_pus.is_empty() {
                cpu_info.recommended_threads()
            } else {
                planned_pus.len() as u32
            }
        } else {
            config.threads
        };
        let mine_threads = threads;
        let worker_pus = planned_pus
            .iter()
            .copied()
            .take(mine_threads as usize)
            .collect::<Vec<_>>();

        let mut pool_url = config.pool_url.clone();
        let mut pool_sni = config.pool_sni.clone();

        if config.doh {
            info!("* DOH          resolving pool address...");
            match parse_pool_host_port(&pool_url) {
                Some((host, port)) => {
                    if pool_sni.is_none() && config.pool_tls {
                        pool_sni = Some(host.clone());
                    }
                    match doh::resolve(&host, port) {
                        Some(addr) => {
                            pool_url = addr.to_string();
                            info!("* DOH          resolved: {}", pool_url);
                        }
                        None => {
                            warn!("* DOH          resolve failed, falling back to system DNS");
                        }
                    }
                }
                None => {
                    warn!("* DOH          invalid pool address, falling back to system DNS");
                }
            }
        }

        info!("* THREADS      {}", mine_threads);
        info!("* THREAD PLAN  {:?}", worker_pus);
        info!("* POOL         {}", pool_url);
        if let Some(sni) = pool_sni.as_deref() {
            info!("* SNI          {}", sni);
        }
        if let Some(proxy) = config.socks5.as_deref() {
            info!("* SOCKS5       {}", proxy);
        }
        if config.pool_tls && config.tls_fingerprint.is_none() {
            warn!(
                "* TLS          certificate is not verified; use --tls-fingerprint to pin the pool certificate"
            );
        }

        let stats = Arc::new(MiningStats::new());

        let (job_tx, job_rx) = watch::channel::<Option<job::Job>>(None);
        let (submit_tx, submit_rx) = mpsc::channel::<(String, String, String)>(256);
        let (event_tx, mut event_rx) = mpsc::channel::<StratumEvent>(256);

        let mut mined_shares = if config.keepalive {
            info!("* KEEPALIVE    connection only, mining disabled");
            mpsc::channel::<MinedShare>(1).1
        } else {
            let miner = Miner::new(mine_threads, worker_pus.clone(), stats.clone());
            miner.start(job_rx).await
        };

        let mut algo_perf = config.algo_perf.clone();
        if !config.keepalive
            && (config.rebench_algo
                || !algo_perf
                    .get(algorithms::RX0)
                    .map(|v| *v > 0.0)
                    .unwrap_or(false))
        {
            info!(
                "* ALGO BENCH   {} {}s",
                algorithms::RX0,
                config.bench_algo_time
            );
            match miner::run_benchmark(
                mine_threads,
                config.bench_algo_time,
                Some(worker_pus.as_slice()),
            ) {
                Ok(hashrate) if hashrate > 0 => {
                    algo_perf.insert(algorithms::RX0.to_string(), hashrate as f64);
                    info!("algo-perf {}={}", algorithms::RX0, hashrate);
                }
                Ok(_) => warn!("algo benchmark produced zero hashrate"),
                Err(e) => warn!("algo benchmark failed: {}", e),
            }
        }

        let stratum = StratumClient::new(
            pool_url,
            config.pool_user.clone(),
            config.pool_pass.clone(),
            config.pool_tls,
            pool_sni,
            config.tls_allow_12,
            config.tls_fingerprint.clone(),
            config.socks5.clone(),
            config.user_agent.clone(),
            config.miner_signature.clone(),
            config.http2,
            config.http3,
            config.ws,
            algo_perf,
            config.algo_min_time,
        );

        let keepalive = config.keepalive;
        let stratum_handle =
            tokio::spawn(async move { stratum.run(job_tx, submit_rx, event_tx, keepalive).await });

        let mut shutdown_rx = install_signal_handler();

        let logged_in = Arc::new(AtomicBool::new(false));
        let stats_logged_in = logged_in.clone();
        let stats_clone = stats.clone();
        let stats_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                interval.tick().await;
                if !stats_logged_in.load(Ordering::Acquire) {
                    continue;
                }
                if stats_clone.total_hashes() == 0 {
                    continue;
                }
                info!(
                    "speed {} {} accepted {} rejected {}s",
                    stats_clone.format_hashrate(),
                    stats_clone.accepted(),
                    stats_clone.rejected(),
                    stats_clone.uptime_secs(),
                );
            }
        });

        let submit_tx_clone = submit_tx.clone();
        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    info!("shutting down...");
                    break;
                }

                Some(event) = event_rx.recv() => {
                    match event {
                        StratumEvent::NewJob(job) => {
                            debug!(
                                "new job: {} diff={} algo={} height={:?}",
                                job.job_id,
                                job.difficulty(),
                                job.algo,
                                job.height
                            );
                        }
                        StratumEvent::Accepted => {
                            stats.record_accepted();
                            debug!("accepted +1");
                        }
                        StratumEvent::Rejected(reason) => {
                            stats.record_rejected();
                            debug!("rejected +1: {}", reason);
                        }
                        StratumEvent::Connected => {
                            logged_in.store(true, Ordering::Release);
                            info!("connected");
                        }
                        StratumEvent::Disconnected(reason) => {
                            logged_in.store(false, Ordering::Release);
                            warn!("disconnected: {}", reason);
                        }
                    }
                }

                Some(share) = mined_shares.recv() => {
                    let job_id = share.job_id.clone();
                    let nonce = share.nonce.clone();
                    let result = share.result.clone();
                    if submit_tx_clone
                        .send((job_id.clone(), nonce.clone(), result))
                        .await
                        .is_err()
                    {
                        warn!("submit queue closed");
                    }
                    debug!("share: job={} nonce={}", job_id, nonce);
                }
            }
        }

        drop(submit_tx);

        stats_handle.abort();
        stratum_handle.abort();

        self.taskbar.set_active(false);
        self.running.store(false, Ordering::Release);
        info!("stopped. total hashes: {}", stats.total_hashes());
        Ok(())
    }

    async fn run_daemon_rpc(&mut self, config: &Config) -> anyhow::Result<()> {
        let cpu_info = CpuInfo::detect();
        cpu_info.print_summary();
        let plan = cpu_info.build_thread_plan();

        let planned_pus = if config.use_e_cores {
            plan.preferred_with_e()
        } else {
            plan.preferred_p_only()
        };
        let threads = if config.threads == 0 {
            if planned_pus.is_empty() {
                cpu_info.recommended_threads()
            } else {
                planned_pus.len() as u32
            }
        } else {
            config.threads
        };
        let worker_pus = planned_pus
            .iter()
            .copied()
            .take(threads as usize)
            .collect::<Vec<_>>();

        let daemon = DaemonRpcClient::new(
            &config.pool_url,
            &config.pool_user,
            config.daemon_rpc_login.clone(),
        );
        info!("* MODE         daemon RPC solo");
        info!("* THREADS      {}", threads);
        info!("* THREAD PLAN  {:?}", worker_pus);
        info!("* DAEMON RPC   {}", daemon.rpc_url());

        let stats = Arc::new(MiningStats::new());
        let (job_tx, job_rx) = watch::channel::<Option<job::Job>>(None);
        let templates = Arc::new(Mutex::new(HashMap::<String, String>::new()));
        let miner = Miner::new(threads, worker_pus, stats.clone());
        let mut mined_shares = miner.start(job_rx).await;

        let daemon_for_jobs = daemon.clone();
        let templates_for_jobs = templates.clone();
        let job_handle = tokio::spawn(async move {
            let mut last_job_id = String::new();
            loop {
                let client = daemon_for_jobs.clone();
                match tokio::task::spawn_blocking(move || client.get_block_template()).await {
                    Ok(Ok(job)) => {
                        if let Some(template) = job.block_template_blob.clone() {
                            templates_for_jobs
                                .lock()
                                .unwrap()
                                .insert(job.job_id.clone(), template);
                        }
                        if job.job_id != last_job_id {
                            info!(
                                "daemon job: id={} height={:?} diff={}",
                                job.job_id,
                                job.height,
                                job.difficulty()
                            );
                            last_job_id = job.job_id.clone();
                            let _ = job_tx.send(Some(job));
                        }
                    }
                    Ok(Err(e)) => warn!("daemon get_block_template failed: {}", e),
                    Err(e) => warn!("daemon get_block_template task failed: {}", e),
                }
                tokio::time::sleep(Duration::from_secs(15)).await;
            }
        });

        let stats_clone = stats.clone();
        let stats_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                interval.tick().await;
                if stats_clone.total_hashes() == 0 {
                    continue;
                }
                info!(
                    "speed {} blocks-submitted {} rejected {} uptime {}s",
                    stats_clone.format_hashrate(),
                    stats_clone.accepted(),
                    stats_clone.rejected(),
                    stats_clone.uptime_secs(),
                );
            }
        });

        let mut shutdown_rx = install_signal_handler();
        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    info!("shutting down...");
                    break;
                }
                Some(share) = mined_shares.recv() => {
                    debug!(
                        "solo candidate: job={} nonce={} hash={}",
                        share.job_id, share.nonce, share.result
                    );
                    let template = templates.lock().unwrap().get(&share.job_id).cloned();
                    let Some(template) = template else {
                        stats.record_rejected();
                        warn!("solo candidate rejected locally: missing block template for {}", share.job_id);
                        continue;
                    };
                    let block_blob = match block_template_with_nonce(&template, &share.nonce) {
                        Ok(blob) => blob,
                        Err(e) => {
                            stats.record_rejected();
                            warn!("solo candidate rejected locally: {}", e);
                            continue;
                        }
                    };
                    let client = daemon.clone();
                    match tokio::task::spawn_blocking(move || client.submit_block(&block_blob)).await {
                        Ok(Ok(())) => {
                            stats.record_accepted();
                            info!("solo block submitted nonce={} hash={}", share.nonce, share.result);
                        }
                        Ok(Err(e)) => {
                            stats.record_rejected();
                            warn!("solo submit_block rejected: {}", e);
                        }
                        Err(e) => {
                            stats.record_rejected();
                            warn!("solo submit_block task failed: {}", e);
                        }
                    }
                }
            }
        }

        job_handle.abort();
        stats_handle.abort();
        self.taskbar.set_active(false);
        self.running.store(false, Ordering::Release);
        info!("stopped. total hashes: {}", stats.total_hashes());
        Ok(())
    }
}

fn install_signal_handler() -> tokio::sync::watch::Receiver<bool> {
    let (tx, rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("received Ctrl+C, shutting down...");
        let _ = tx.send(true);
    });
    rx
}

fn parse_pool_host_port(url: &str) -> Option<(String, u16)> {
    let raw = url.trim();
    if raw.is_empty() {
        return None;
    }

    let rest = raw.split_once("://").map(|(_, rest)| rest).unwrap_or(raw);
    let authority = match rest.find(|c| matches!(c, '/' | '?' | '#')) {
        Some(idx) => &rest[..idx],
        None => rest,
    };

    if let Some(stripped) = authority.strip_prefix('[') {
        let end = stripped.find(']')?;
        let host = &stripped[..end];
        let port = stripped[end + 1..].strip_prefix(':')?.parse::<u16>().ok()?;
        return Some((host.to_string(), port));
    }

    let (host, port) = authority.rsplit_once(':')?;
    if host.is_empty() || host.contains(':') {
        return None;
    }

    Some((host.to_string(), port.parse::<u16>().ok()?))
}

fn block_template_with_nonce(template_hex: &str, nonce_hex: &str) -> anyhow::Result<String> {
    let mut template = hex::decode(template_hex)?;
    let nonce = hex::decode(nonce_hex)?;
    if nonce.len() != 4 {
        anyhow::bail!("invalid nonce length: {} bytes", nonce.len());
    }
    if job::NONCE_OFFSET + 4 > template.len() {
        anyhow::bail!("block template is too short for nonce offset");
    }
    template[job::NONCE_OFFSET..job::NONCE_OFFSET + 4].copy_from_slice(&nonce);
    Ok(hex::encode(template))
}
