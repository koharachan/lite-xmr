//! 挖矿 worker 实现。

use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::thread;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info};

use lite_xmr_core::{Job, MiningStats};

/// 挖矿结果。
#[derive(Debug, Clone)]
pub struct MinedShare {
    /// 任务 ID
    pub job_id: String,
    /// Nonce (十六进制)
    pub nonce: String,
    /// 结果哈希 (十六进制)
    pub result: String,
}

/// 挖矿池：管理多个 worker 线程。
pub struct MiningPool {
    thread_count: u32,
    stats: Arc<MiningStats>,
}

impl MiningPool {
    /// 创建新的挖矿池。
    pub fn new(thread_count: u32, stats: Arc<MiningStats>) -> Self {
        MiningPool { thread_count, stats }
    }

    /// 启动挖矿池。
    ///
    /// 返回一个 channel receiver 用于接收挖到的份额。
    pub async fn start(
        &self,
        job_rx: watch::Receiver<Option<Job>>,
    ) -> mpsc::Receiver<MinedShare> {
        let (submit_tx, submit_rx) = mpsc::channel::<MinedShare>(256);

        // 为每个线程启动一个 worker
        for worker_id in 0..self.thread_count {
            let submit_tx = submit_tx.clone();
            let stats = self.stats.clone();

            // 创建同步 channel 用于向工作线程发送任务
            let (job_tx, job_rx_sync) = std_mpsc::channel::<Option<Job>>();

            // 将 tokio watch channel 桥接到同步 channel
            let mut job_rx_clone = job_rx.clone();
            let rt_handle = tokio::runtime::Handle::current();
            thread::spawn(move || {
                let mut current_job: Option<Job> = None;
                loop {
                    match rt_handle.block_on(job_rx_clone.changed()) {
                        Ok(()) => {
                            let job_ref = job_rx_clone.borrow_and_update();
                            let new_job = job_ref.clone();
                            // 只有当任务实际变化时才发送
                            if new_job.as_ref().map(|j| &j.job_id) != current_job.as_ref().map(|j| &j.job_id) {
                                current_job = new_job.clone();
                                if job_tx.send(new_job).is_err() {
                                    break;
                                }
                            }
                        }
                        Err(_) => {
                            let _ = job_tx.send(None);
                            break;
                        }
                    }
                }
            });

            // 启动实际的挖矿工作线程（使用 std::thread，因为 RandomXVM 不是 Send）
            thread::spawn(move || {
                worker_loop_sync(worker_id, job_rx_sync, submit_tx, stats);
            });
        }

        info!("已启动 {} 个挖矿线程", self.thread_count);
        submit_rx
    }
}

/// 单个 worker 的同步主循环（在独立线程中运行）。
fn worker_loop_sync(
    worker_id: u32,
    job_rx: std_mpsc::Receiver<Option<Job>>,
    submit_tx: mpsc::Sender<MinedShare>,
    stats: Arc<MiningStats>,
) {
    info!("Worker #{} 已启动", worker_id);

    // 初始化 RandomX 缓存和虚拟机
    let mut current_seed: Option<String> = None;
    let mut vm: Option<randomx_rs::RandomXVM> = None;

    loop {
        // 等待新任务
        let job = match job_rx.recv() {
            Ok(Some(job)) => job,
            Ok(None) => {
                debug!("Worker #{} 收到停止信号", worker_id);
                break;
            }
            Err(_) => {
                debug!("Worker #{} 任务 channel 已关闭", worker_id);
                break;
            }
        };

        // 检查是否需要重新初始化 RandomX (seed 变化时)
        let seed = job.seed_hash.clone().unwrap_or_default();
        if current_seed.as_ref() != Some(&seed) {
            debug!("Worker #{} 检测到新 seed，重新初始化 RandomX", worker_id);

            // 解码 seed hash
            let seed_bytes = match hex::decode(&seed) {
                Ok(b) if !b.is_empty() => b,
                _ => vec![0u8; 32],
            };

            // 创建新的缓存
            match randomx_rs::RandomXCache::new(randomx_rs::RandomXFlag::default(), &seed_bytes) {
                Ok(new_cache) => {
                    // 创建虚拟机 (light mode: 只使用 cache，不使用 dataset)
                    match randomx_rs::RandomXVM::new(
                        randomx_rs::RandomXFlag::default(),
                        Some(new_cache),
                        None,
                    ) {
                        Ok(new_vm) => {
                            vm = Some(new_vm);
                            current_seed = Some(seed);
                            debug!("Worker #{} RandomX 初始化完成", worker_id);
                        }
                        Err(e) => {
                            error!("Worker #{} RandomX VM 创建失败: {}", worker_id, e);
                            continue;
                        }
                    }
                }
                Err(e) => {
                    error!("Worker #{} RandomX Cache 创建失败: {}", worker_id, e);
                    continue;
                }
            }
        }

        // 执行挖矿
        if let Some(ref vm) = vm {
            mine_loop_sync(worker_id, vm, &job, &submit_tx, &stats);
        }
    }
}

/// 挖矿循环：在当前任务上不断计算哈希（同步版本）。
fn mine_loop_sync(
    worker_id: u32,
    vm: &randomx_rs::RandomXVM,
    job: &Job,
    submit_tx: &mpsc::Sender<MinedShare>,
    stats: &MiningStats,
) {
    let mut blob = job.blob_bytes();
    let nonce_offset = job.nonce_offset;
    let target_diff = job.target_difficulty();

    let mut nonce: u32 = worker_id;

    loop {
        // 将 nonce 写入 blob
        if nonce_offset + 4 <= blob.len() {
            blob[nonce_offset..nonce_offset + 4].copy_from_slice(&nonce.to_le_bytes());
        }

        // 计算 RandomX 哈希
        let hash = match vm.calculate_hash(&blob) {
            Ok(h) => h,
            Err(e) => {
                error!("Worker #{} 哈希计算错误: {}", worker_id, e);
                break;
            }
        };

        stats.record_hashes(1);

        // 检查是否满足难度
        if check_hash_difficulty(&hash, target_diff) {
            info!(
                "Worker #{} 找到份额! nonce={:08x} job={}",
                worker_id, nonce, job.job_id
            );

            let share = MinedShare {
                job_id: job.job_id.clone(),
                nonce: format!("{:08x}", nonce),
                result: hex::encode(&hash),
            };

            if submit_tx.try_send(share).is_err() {
                debug!("Worker #{} 提交 channel 已满", worker_id);
            }

            stats.record_accepted();
        }

        nonce += 1; // 每个 worker 使用不同的 nonce 起始值
    }
}

/// 检查哈希是否满足目标难度。
fn check_hash_difficulty(hash: &[u8], target_difficulty: u64) -> bool {
    if hash.len() < 4 {
        return false;
    }

    // 取哈希的前 4 字节作为难度 (小端)
    let hash_diff = u32::from_le_bytes([hash[0], hash[1], hash[2], hash[3]]);

    if hash_diff == 0 {
        return true;
    }

    // hash_diff 越小越好，所以 hash_diff <= (2^32 / target_difficulty)
    let threshold = if target_difficulty > 0 {
        (u64::MAX / target_difficulty) as u32
    } else {
        0
    };

    hash_diff <= threshold
}
