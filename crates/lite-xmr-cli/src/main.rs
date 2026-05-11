//! lite-xmr: 轻量级 Monero (XMR) CPU 矿工。
//!
//! 使用 Rust 编写的高性能 RandomX 挖矿客户端。
//! - 纯 Rust TLS (rustls)，不依赖 OpenSSL
//! - tokio 异步运行时
//! - randomx-rs 核心

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};

use lite_xmr_core::{Config, CpuInfo, MiningStats};
use lite_xmr_miner::MiningPool;
use lite_xmr_stratum::{StratumClient, StratumEvent};

/// 打印启动 banner。
fn print_banner() {
    info!("╔══════════════════════════════════════════════╗");
    info!("║         lite-xmr v0.1.0                     ║");
    info!("║   轻量级 Monero CPU 矿工 (Rust)            ║");
    info!("║   无抽水 · 无追踪 · 纯粹挖矿               ║");
    info!("╚══════════════════════════════════════════════╝");
}

/// 安装信号处理器，返回一个用于检测 Ctrl+C 的 channel。
fn install_signal_handler() -> tokio::sync::watch::Receiver<bool> {
    let (tx, rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("收到 Ctrl+C 信号，正在优雅关闭...");
        let _ = tx.send(true);
    });
    rx
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 解析命令行参数
    let args = lite_xmr_core::config::Args::parse()?;
    let config = Config::load(&args)?;

    // 初始化日志
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

    // 检测 CPU 信息
    let cpu_info = CpuInfo::detect();
    cpu_info.print_summary();

    // 确定线程数
    let threads = if config.threads == 0 {
        cpu_info.recommended_threads()
    } else {
        config.threads
    };
    info!("使用 {} 个挖矿线程", threads);

    // 初始化统计
    let stats = Arc::new(MiningStats::new());

    // 创建 channel
    let (job_tx, job_rx) = watch::channel::<Option<lite_xmr_core::Job>>(None);
    let (submit_tx, submit_rx) = mpsc::channel::<(String, String, String)>(256);
    let (event_tx, mut event_rx) = mpsc::channel::<StratumEvent>(256);

    // 启动挖矿池
    let miner = MiningPool::new(threads, stats.clone());
    let mut mined_shares = miner.start(job_rx).await;

    // 启动 Stratum 客户端
    let stratum = StratumClient::new(
        config.pool_url.clone(),
        config.pool_user.clone(),
        config.pool_pass.clone(),
        config.pool_tls,
    );

    let stratum_handle = tokio::spawn(async move {
        stratum.run(job_tx, submit_rx, event_tx).await
    });

    // 安装信号处理
    let mut shutdown_rx = install_signal_handler();

    // 启动统计打印循环
    let stats_clone = stats.clone();
    let stats_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            info!(
                "算力: {} | 已接受: {} | 已拒绝: {} | 运行时间: {}s",
                stats_clone.format_hashrate(),
                stats_clone.accepted(),
                stats_clone.rejected(),
                stats_clone.uptime_secs(),
            );
        }
    });

    // 主事件循环
    loop {
        tokio::select! {
            // 检查关闭信号
            _ = shutdown_rx.changed() => {
                info!("正在关闭...");
                break;
            }

            // 处理 Stratum 事件
            Some(event) = event_rx.recv() => {
                match event {
                    StratumEvent::NewJob(job) => {
                        debug!("新任务: {} (高度: {:?})", job.job_id, job.height);
                    }
                    StratumEvent::Accepted => {
                        info!("✓ 份额已被接受");
                    }
                    StratumEvent::Rejected(reason) => {
                        warn!("✗ 份额被拒绝: {}", reason);
                        stats.record_rejected();
                    }
                    StratumEvent::Connected => {
                        info!("已连接到矿池");
                    }
                    StratumEvent::Disconnected(reason) => {
                        warn!("与矿池断开连接: {}", reason);
                    }
                }
            }

            // 处理挖到的份额
            Some(share) = mined_shares.recv() => {
                info!(
                    "提交份额: job={}, nonce={}",
                    share.job_id, share.nonce
                );
            }
        }
    }

    // 清理
    stats_handle.abort();
    stratum_handle.abort();

    info!("lite-xmr 已停止。总哈希: {}", stats.total_hashes());
    Ok(())
}
