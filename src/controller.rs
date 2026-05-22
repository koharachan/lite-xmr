use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::cpu::{os_name, CpuInfo, APP_VERSION};
use crate::doh;
use crate::job;
use crate::miner::{Miner, MinedShare};
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
        info!(" * ABOUT        lite-xmr/{} {} x86-64", APP_VERSION, os_name());
    }

    pub async fn run(&mut self, config: &Config) -> anyhow::Result<()> {
        self.running.store(true, Ordering::Release);
        self.taskbar.set_active(true);
        self.taskbar.set_enabled(true);

        self.print_banner();

        let cpu_info = CpuInfo::detect();
        cpu_info.print_summary();

        let threads = if config.threads == 0 {
            cpu_info.recommended_threads()
        } else {
            config.threads
        };

        let _donate_level = 0u32;
        let _donate_threads = (threads as f64 * (_donate_level as f64 / 100.0)).ceil() as u32;
        let _mine_threads = threads.saturating_sub(_donate_threads);

        let mine_threads = threads;

        let mut pool_url = config.pool_url.clone();

        if config.doh {
            info!("   * DOH          正在解析矿池地址...");
            let (host, port) = match pool_url.rsplit_once(':') {
                Some((h, p)) => {
                    let h = h.trim_start_matches('[').trim_end_matches(']');
                    (h, p.parse::<u16>().unwrap_or(0))
                }
                None => (pool_url.as_str(), 0),
            };
            match doh::resolve(host, port) {
                Some(addr) => {
                    pool_url = addr.to_string();
                    info!("   * DOH          已解析: {}", pool_url);
                }
                None => {
                    warn!("   * DOH          解析失败，回退到系统 DNS");
                }
            }
        }

        info!("   * THREADS      {}", mine_threads);
        info!("   * POOL         {}", pool_url);

        let stats = Arc::new(MiningStats::new());

        let (job_tx, job_rx) = watch::channel::<Option<job::Job>>(None);
        let (submit_tx, submit_rx) = mpsc::channel::<(String, String, String)>(256);
        let (event_tx, mut event_rx) = mpsc::channel::<StratumEvent>(256);

        let mut mined_shares = if config.keepalive {
            info!("   * KEEPALIVE    保持连接活跃，不挖矿");
            mpsc::channel::<MinedShare>(1).1
        } else {
            info!("   * DONATE       0%");
            let miner = Miner::new(mine_threads, stats.clone());
            miner.start(job_rx).await
        };

        let stratum = StratumClient::new(
            pool_url,
            config.pool_user.clone(),
            config.pool_pass.clone(),
            config.pool_tls,
        );

        let keepalive = config.keepalive;
        let stratum_handle = tokio::spawn(async move {
            stratum.run(job_tx, submit_rx, event_tx, keepalive).await
        });

        let mut shutdown_rx = install_signal_handler();

        let stats_clone = stats.clone();
        let stats_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                interval.tick().await;
                info!(
                    "speed {}/{} accepted/{} rejected/{}s",
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
                    info!("正在关闭...");
                    break;
                }

                Some(event) = event_rx.recv() => {
                    match event {
                        StratumEvent::NewJob(job) => {
                            let height = job.height;
                            let _target = &*job.target;
                            debug!("new job: {} diff={} algo={} height={:?}",
                                job.job_id, job.difficulty(), job.algo, height);
                        }
                        StratumEvent::Accepted => {
                            stats.record_accepted();
                            info!("accepted");
                        }
                        StratumEvent::Rejected(reason) => {
                            warn!("rejected: {}", reason);
                            stats.record_rejected();
                        }
                        StratumEvent::Connected => {
                            info!("connected");
                        }
                        StratumEvent::Disconnected(reason) => {
                            warn!("disconnected: {}", reason);
                        }
                    }
                }

                Some(share) = mined_shares.recv() => {
                    let job_id = share.job_id.clone();
                    let nonce = share.nonce.clone();
                    let result = share.result.clone();
                    let _ = submit_tx_clone.try_send((job_id.clone(), nonce.clone(), result));
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
        info!("收到 Ctrl+C，正在关闭...");
        let _ = tx.send(true);
    });
    rx
}
