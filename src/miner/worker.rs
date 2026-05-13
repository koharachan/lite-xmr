use std::sync::mpsc as std_mpsc;
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

pub struct MiningPool {
    thread_count: u32,
    stats: Arc<MiningStats>,
}

impl MiningPool {
    pub fn new(thread_count: u32, stats: Arc<MiningStats>) -> Self {
        MiningPool { thread_count, stats }
    }

    pub async fn start(
        &self,
        job_rx: watch::Receiver<Option<Job>>,
    ) -> mpsc::Receiver<MinedShare> {
        let (submit_tx, submit_rx) = mpsc::channel::<MinedShare>(256);

        for worker_id in 0..self.thread_count {
            let submit_tx = submit_tx.clone();
            let stats = self.stats.clone();

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

            thread::spawn(move || {
                worker_loop_sync(worker_id, job_rx_sync, submit_tx, stats);
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
) {
    info!("Worker #{} 已启动", worker_id);

    let mut current_seed: Option<String> = None;
    let mut vm: Option<randomx_rs::RandomXVM> = None;

    loop {
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

        let seed = job.seed_hash.clone().unwrap_or_default();
        if current_seed.as_ref() != Some(&seed) {
            debug!("Worker #{} 检测到新 seed，重新初始化 RandomX", worker_id);

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

        if let Some(ref vm) = vm {
            mine_loop_sync(worker_id, vm, &job, &submit_tx, &stats);
        }
    }
}

fn mine_loop_sync(
    worker_id: u32,
    vm: &randomx_rs::RandomXVM,
    job: &Job,
    submit_tx: &mpsc::Sender<MinedShare>,
    stats: &MiningStats,
) {
    let mut blob = job.blob_bytes().to_vec();
    let nonce_offset = job::NONCE_OFFSET;
    let target_diff = job.difficulty();

    let mut nonce: u32 = worker_id;

    loop {
        if nonce_offset + 4 <= blob.len() {
            blob[nonce_offset..nonce_offset + 4].copy_from_slice(&nonce.to_le_bytes());
        }

        let hash = match vm.calculate_hash(&blob) {
            Ok(h) => h,
            Err(e) => {
                error!("Worker #{} 哈希计算错误: {}", worker_id, e);
                break;
            }
        };

        stats.record_hashes(1);

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

        nonce += 1;
    }
}

fn check_hash_difficulty(hash: &[u8], target_difficulty: u64) -> bool {
    if hash.len() < 8 || target_difficulty == 0 {
        return false;
    }
    let hash_val = u64::from_le_bytes([hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7]]);
    hash_val < u64::MAX / target_difficulty
}
