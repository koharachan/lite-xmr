use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Instant;

use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info};

use crate::job::{self, Job};
use crate::stats::MiningStats;

#[derive(Clone)]
struct SharedDataset(randomx_rs::RandomXDataset);

unsafe impl Send for SharedDataset {}
unsafe impl Sync for SharedDataset {}

enum DatasetState {
    Empty,
    Building {
        seed: String,
    },
    Ready {
        seed: String,
        dataset: SharedDataset,
    },
    Failed {
        seed: String,
    },
}

struct RxDatasetCache {
    state: Mutex<DatasetState>,
    ready: Condvar,
}

impl RxDatasetCache {
    fn new() -> Self {
        RxDatasetCache {
            state: Mutex::new(DatasetState::Empty),
            ready: Condvar::new(),
        }
    }

    fn get_or_build(&self, seed: &str, seed_bytes: &[u8]) -> Option<SharedDataset> {
        let mut state = self.state.lock().unwrap();
        loop {
            match &*state {
                DatasetState::Ready {
                    seed: cached_seed,
                    dataset,
                } if cached_seed == seed => return Some(dataset.clone()),
                DatasetState::Failed { seed: failed_seed } if failed_seed == seed => return None,
                DatasetState::Building {
                    seed: building_seed,
                } if building_seed == seed => {
                    state = self.ready.wait(state).unwrap();
                }
                _ => {
                    *state = DatasetState::Building {
                        seed: seed.to_string(),
                    };
                    break;
                }
            }
        }
        drop(state);

        let started = Instant::now();
        info!("RandomX dataset initializing seed={} ...", short_seed(seed));
        let dataset = build_full_dataset(seed_bytes);

        let mut state = self.state.lock().unwrap();
        match dataset {
            Ok(dataset) => {
                let dataset = SharedDataset(dataset);
                *state = DatasetState::Ready {
                    seed: seed.to_string(),
                    dataset: dataset.clone(),
                };
                self.ready.notify_all();
                info!(
                    "RandomX dataset ready seed={} ({:.1}s)",
                    short_seed(seed),
                    started.elapsed().as_secs_f64()
                );
                Some(dataset)
            }
            Err(e) => {
                *state = DatasetState::Failed {
                    seed: seed.to_string(),
                };
                self.ready.notify_all();
                error!(
                    "RandomX dataset init failed seed={}: {}; falling back to light mode",
                    short_seed(seed),
                    e
                );
                None
            }
        }
    }
}

fn build_full_dataset(
    seed_bytes: &[u8],
) -> Result<randomx_rs::RandomXDataset, randomx_rs::RandomXError> {
    let cache_flags = randomx_rs::RandomXFlag::get_recommended_flags();
    let cache = randomx_rs::RandomXCache::new(cache_flags, seed_bytes)?;
    randomx_rs::RandomXDataset::new(randomx_rs::RandomXFlag::FLAG_DEFAULT, cache, 0)
}

fn short_seed(seed: &str) -> &str {
    seed.get(..8).unwrap_or(seed)
}

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

    pub async fn start(&self, job_rx: watch::Receiver<Option<Job>>) -> mpsc::Receiver<MinedShare> {
        let (submit_tx, submit_rx) = mpsc::channel::<MinedShare>(256);
        let dataset_cache = Arc::new(RxDatasetCache::new());

        for worker_id in 0..self.thread_count {
            let submit_tx = submit_tx.clone();
            let stats = self.stats.clone();
            let thread_count = self.thread_count;
            let dataset_cache = dataset_cache.clone();

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
                worker_loop_sync(
                    worker_id,
                    job_rx_sync,
                    submit_tx,
                    stats,
                    thread_count,
                    dataset_cache,
                );
            });
        }

        info!("started {} mining threads", self.thread_count);
        submit_rx
    }
}

fn worker_loop_sync(
    worker_id: u32,
    job_rx: std_mpsc::Receiver<Option<Job>>,
    submit_tx: mpsc::Sender<MinedShare>,
    stats: Arc<MiningStats>,
    thread_count: u32,
    dataset_cache: Arc<RxDatasetCache>,
) {
    info!("Worker #{} started", worker_id);

    let mut current_seed: Option<String> = None;
    let mut vm: Option<randomx_rs::RandomXVM> = None;
    let mut current_job: Option<Job> = None;
    let mut nonce: u32 = worker_id;

    loop {
        if let Ok(Some(new_job)) = job_rx.try_recv() {
            current_job = Some(new_job);
            nonce = worker_id;
        } else if current_job.is_none() {
            match job_rx.recv() {
                Ok(Some(job)) => {
                    current_job = Some(job);
                    nonce = worker_id;
                }
                Ok(None) | Err(_) => break,
            }
        }

        let job = match &current_job {
            Some(j) => j,
            None => continue,
        };

        let seed = job.seed_hash.clone().unwrap_or_default();
        if current_seed.as_ref() != Some(&seed) {
            debug!("Worker #{} new seed, initializing RandomX", worker_id);
            let seed_bytes = match hex::decode(&seed) {
                Ok(b) if !b.is_empty() => b,
                _ => vec![0u8; 32],
            };

            match create_randomx_vm(&seed, &seed_bytes, &dataset_cache) {
                Ok(new_vm) => {
                    vm = Some(new_vm);
                    current_seed = Some(seed);
                    debug!("Worker #{} RandomX ready", worker_id);
                }
                Err(e) => {
                    error!("Worker #{} failed to create VM: {}", worker_id, e);
                    continue;
                }
            }
        }

        if let Some(ref vm) = vm {
            mine_one_batch(
                worker_id,
                vm,
                job,
                &submit_tx,
                &stats,
                thread_count,
                &mut nonce,
            );
        }
    }
}

fn create_randomx_vm(
    seed: &str,
    seed_bytes: &[u8],
    dataset_cache: &RxDatasetCache,
) -> Result<randomx_rs::RandomXVM, randomx_rs::RandomXError> {
    let flags = randomx_rs::RandomXFlag::get_recommended_flags();
    if let Some(dataset) = dataset_cache.get_or_build(seed, seed_bytes) {
        let full_mem_flags = flags | randomx_rs::RandomXFlag::FLAG_FULL_MEM;
        match randomx_rs::RandomXVM::new(full_mem_flags, None, Some(dataset.0.clone())) {
            Ok(vm) => return Ok(vm),
            Err(e) => {
                error!(
                    "RandomX full-mem VM failed seed={}: {}; falling back to light mode",
                    short_seed(seed),
                    e
                );
            }
        }
    }

    let cache = randomx_rs::RandomXCache::new(flags, seed_bytes)?;
    randomx_rs::RandomXVM::new(flags, Some(cache), None)
}

fn mine_one_batch(
    worker_id: u32,
    vm: &randomx_rs::RandomXVM,
    job: &Job,
    submit_tx: &mpsc::Sender<MinedShare>,
    stats: &MiningStats,
    thread_count: u32,
    nonce: &mut u32,
) {
    const BATCH_SIZE: u32 = 1024;

    let mut blob = job.blob_bytes().to_vec();
    let nonce_offset = job::NONCE_OFFSET;
    let target_diff = job.difficulty();

    for _ in 0..BATCH_SIZE {
        if nonce_offset + 4 <= blob.len() {
            blob[nonce_offset..nonce_offset + 4].copy_from_slice(&(*nonce).to_le_bytes());
        }

        let hash = match vm.calculate_hash(&blob) {
            Ok(h) => h,
            Err(e) => {
                error!("Worker #{} hash error: {}", worker_id, e);
                break;
            }
        };

        stats.record_hashes(1);

        if check_hash_difficulty(&hash, target_diff) {
            let share = MinedShare {
                job_id: job.job_id.clone(),
                nonce: job::format_nonce(*nonce),
                result: hex::encode(&hash),
            };
            let _ = submit_tx.try_send(share);
        }

        *nonce = (*nonce).wrapping_add(thread_count);
    }
}

fn check_hash_difficulty(hash: &[u8], target_difficulty: u64) -> bool {
    if hash.len() < 8 || target_difficulty == 0 {
        return false;
    }

    let hash_val = u64::from_le_bytes([
        hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7],
    ]);
    hash_val < u64::MAX / target_difficulty
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn difficulty_check_accepts_hashes_below_target() {
        let difficulty = 2;
        let target = u64::MAX / difficulty;
        let hash = (target - 1).to_le_bytes();

        assert!(check_hash_difficulty(&hash, difficulty));
    }

    #[test]
    fn difficulty_check_rejects_hashes_at_or_above_target() {
        let difficulty = 2;
        let target = u64::MAX / difficulty;
        let hash = target.to_le_bytes();

        assert!(!check_hash_difficulty(&hash, difficulty));
    }
}
