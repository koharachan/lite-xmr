use std::sync::mpsc as std_mpsc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info};

use crate::job::{self, Job};
use crate::stats::MiningStats;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct MinedShare {
    pub job_id: String,
    pub nonce: String,
    pub result: String,
}

pub struct Miner {
    thread_count: u32,
    stats: Arc<MiningStats>,
    #[allow(dead_code)]
    enabled: AtomicBool,
}

impl Miner {
    pub fn new(thread_count: u32, stats: Arc<MiningStats>) -> Self {
        Miner {
            thread_count,
            stats,
            enabled: AtomicBool::new(true),
        }
    }

    #[allow(dead_code)]
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
    }

    #[allow(dead_code)]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    #[allow(dead_code)]
    pub fn thread_count(&self) -> u32 {
        self.thread_count
    }

    pub async fn start(
        &self,
        job_rx: watch::Receiver<Option<Job>>,
    ) -> mpsc::Receiver<MinedShare> {
        let (submit_tx, submit_rx) = mpsc::channel::<MinedShare>(256);

        for worker_id in 0..self.thread_count {
            let submit_tx = submit_tx.clone();
            let stats = self.stats.clone();
            let thread_count = self.thread_count;

            let (job_tx, job_rx_sync) = std_mpsc::channel::<Option<Job>>();

            let mut job_rx_clone = job_rx.clone();
            let rt_handle = tokio::runtime::Handle::current();
            thread::spawn(move || {
                let mut current_job: Option<Job> = None;
                loop {
                    match rt_handle.block_on(job_rx_clone.changed()) {
                        Ok(()) => {
                            let job_ref = job_rx_clone.borrow_and_update();
                            let new_job = job_ref.clone();
                            if new_job.as_ref().map(|j| &j.job_id)
                                != current_job.as_ref().map(|j| &j.job_id)
                            {
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

            thread::spawn(move || {
                worker_loop_sync(worker_id, job_rx_sync, submit_tx, stats, thread_count);
            });
        }

        info!("已启动 {} 个挖矿线程", self.thread_count);
        submit_rx
    }
}

fn worker_loop_sync(
    worker_id: u32,
    job_rx: std_mpsc::Receiver<Option<Job>>,
    submit_tx: mpsc::Sender<MinedShare>,
    stats: Arc<MiningStats>,
    thread_count: u32,
) {
    info!("Worker #{} 已启动", worker_id);

    let mut current_seed: Option<String> = None;
    let mut vm: Option<randomx_rs::RandomXVM> = None;
    let mut current_job: Option<Job> = None;

    loop {
        // 检查是否有新任务（非阻塞）
        if let Ok(Some(new_job)) = job_rx.try_recv() {
            current_job = Some(new_job);
        } else if current_job.is_none() {
            // 还没有任务，阻塞等待
            match job_rx.recv() {
                Ok(Some(job)) => current_job = Some(job),
                Ok(None) | Err(_) => break,
            }
        }

        let job = match &current_job {
            Some(j) => j,
            None => continue,
        };

        // 初始化 / 更新 RandomX VM
        let seed = job.seed_hash.clone().unwrap_or_default();
        if current_seed.as_ref() != Some(&seed) {
            debug!("Worker #{} 新 seed，初始化 RandomX", worker_id);
            let seed_bytes = match hex::decode(&seed) {
                Ok(b) if !b.is_empty() => b,
                _ => vec![0u8; 32],
            };
            match randomx_rs::RandomXCache::new(randomx_rs::RandomXFlag::default(), &seed_bytes) {
                Ok(new_cache) => {
                    match randomx_rs::RandomXVM::new(
                        randomx_rs::RandomXFlag::default(),
                        Some(new_cache),
                        None,
                    ) {
                        Ok(new_vm) => {
                            vm = Some(new_vm);
                            current_seed = Some(seed);
                            debug!("Worker #{} RandomX 就绪", worker_id);
                        }
                        Err(e) => {
                            error!("Worker #{} VM 创建失败: {}", worker_id, e);
                            continue;
                        }
                    }
                }
                Err(e) => {
                    error!("Worker #{} Cache 创建失败: {}", worker_id, e);
                    continue;
                }
            }
        }

        if let Some(ref vm) = vm {
            mine_one_batch(worker_id, vm, job, &submit_tx, &stats, thread_count);
        }
    }
}

/// 挖一小批 hash（约 1024 次），然后返回检查新任务。
/// 使用 thread_count 作为 nonce 步长，避免线程间 nonce 重叠。
fn mine_one_batch(
    worker_id: u32,
    vm: &randomx_rs::RandomXVM,
    job: &Job,
    submit_tx: &mpsc::Sender<MinedShare>,
    stats: &MiningStats,
    thread_count: u32,
) {
    const BATCH_SIZE: u32 = 1024;

    let mut blob = job.blob_bytes().to_vec();
    let nonce_offset = job::NONCE_OFFSET;
    let target_diff = job.difficulty();
    let mut nonce: u32 = worker_id;

    for _ in 0..BATCH_SIZE {
        if nonce_offset + 4 <= blob.len() {
            blob[nonce_offset..nonce_offset + 4].copy_from_slice(&nonce.to_le_bytes());
        }

        let hash = match vm.calculate_hash(&blob) {
            Ok(h) => h,
            Err(e) => {
                error!("Worker #{} hash 错误: {}", worker_id, e);
                break;
            }
        };

        stats.record_hashes(1);

        if check_hash_difficulty(&hash, target_diff) {
            let share = MinedShare {
                job_id: job.job_id.clone(),
                nonce: format!("{:08x}", nonce),
                result: hex::encode(&hash),
            };
            // try_send 失败说明 channel 满了，丢弃当前 share 继续
            let _ = submit_tx.try_send(share);
        }

        nonce = nonce.wrapping_add(thread_count);
    }
}

fn check_hash_difficulty(hash: &[u8], target_difficulty: u64) -> bool {
    if hash.len() < 8 || target_difficulty == 0 {
        return false;
    }
    let hash_val =
        u64::from_le_bytes([hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7]]);
    hash_val < u64::MAX / target_difficulty
}
