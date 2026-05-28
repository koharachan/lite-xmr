use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::cpu::{APP_VERSION, CpuInfo, os_name};
use crate::doh;
use crate::job;
use crate::miner::{MinedShare, Miner};
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

        let stats = Arc::new(MiningStats::new());

        let (job_tx, job_rx) = watch::channel::<Option<job::Job>>(None);
        let (submit_tx, submit_rx) = mpsc::channel::<(String, String, String)>(256);
        let (event_tx, mut event_rx) = mpsc::channel::<StratumEvent>(256);

        let mut mined_shares = if config.keepalive {
            info!("* KEEPALIVE    connection only, mining disabled");
            mpsc::channel::<MinedShare>(1).1
        } else {
            let miner = Miner::new(mine_threads, worker_pus, stats.clone());
            miner.start(job_rx).await
        };

        let stratum = StratumClient::new(
            pool_url,
            config.pool_user.clone(),
            config.pool_pass.clone(),
            config.pool_tls,
            pool_sni,
            config.user_agent.clone(),
            config.http2,
            config.http3,
            config.ws,
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
