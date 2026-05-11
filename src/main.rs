mod config;
mod cpu;
mod error;
mod job;
mod miner;
mod stats;
mod stratum;

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use config::{Args, Config, EarlyExit};
use cpu::{os_name, CpuInfo, APP_VERSION};
use miner::MiningPool;
use stats::MiningStats;
use stratum::{StratumClient, StratumEvent};

fn print_banner() {
    info!(" * ABOUT        lite-xmr/{} {} x86-64", APP_VERSION, os_name());
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = match Args::parse()? {
        Ok(a) => a,
        Err(EarlyExit::Help) | Err(EarlyExit::Version) => return Ok(()),
    };
    let config = Config::load(&args)?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| config.log_level.clone().parse().unwrap()),
        )
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .init();

    print_banner();

    let cpu_info = CpuInfo::detect();
    cpu_info.print_summary();

    let threads = if config.threads == 0 {
        cpu_info.recommended_threads()
    } else {
        config.threads
    };
    info!("   * THREADS      {}", threads);

    info!("   * POOL         {}", config.pool_url);

    let stats = Arc::new(MiningStats::new());

    let (job_tx, job_rx) = watch::channel::<Option<job::Job>>(None);
    let (_submit_tx, submit_rx) = mpsc::channel::<(String, String, String)>(256);
    let (event_tx, mut event_rx) = mpsc::channel::<StratumEvent>(256);

    let miner = MiningPool::new(threads, stats.clone());
    let mut mined_shares = miner.start(job_rx).await;

    let stratum = StratumClient::new(
        config.pool_url.clone(),
        config.pool_user.clone(),
        config.pool_pass.clone(),
        config.pool_tls,
    );

    let stratum_handle = tokio::spawn(async move {
        stratum.run(job_tx, submit_rx, event_tx).await
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

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                info!("正在关闭...");
                break;
            }

            Some(event) = event_rx.recv() => {
                match event {
                    StratumEvent::NewJob(job) => {
                        debug!("new job: {} height={:?}", job.job_id, job.height);
                    }
                    StratumEvent::Accepted => {
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
                info!("share: job={} nonce={}", share.job_id, share.nonce);
            }
        }
    }

    stats_handle.abort();
    stratum_handle.abort();

    info!("stopped. total hashes: {}", stats.total_hashes());
    Ok(())
}
