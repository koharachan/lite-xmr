use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info};

use crate::job::{self, Job};
use crate::randomx;
use crate::stats::MiningStats;

fn pin_current_thread(pu: Option<usize>) {
    use hwlocality::cpu::binding::CpuBindingFlags;
    use hwlocality::cpu::cpuset::CpuSet;
    let Some(pu) = pu else {
        return;
    };
    let topo = match hwlocality::Topology::new() {
        Ok(t) => t,
        Err(e) => {
            debug!("hwloc init failed for affinity bind: {}", e);
            return;
        }
    };
    let mut set = CpuSet::new();
    set.set(pu);
    if let Err(e) = topo.bind_cpu(&set, CpuBindingFlags::THREAD) {
        debug!("bind worker to PU {} failed: {}", pu, e);
    }
}

enum DatasetState {
    Empty,
    Building {
        seed: String,
    },
    Ready {
        seed: String,
        dataset: Arc<randomx::Dataset>,
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

    fn get_or_build(&self, seed: &str, seed_bytes: &[u8]) -> Option<Arc<randomx::Dataset>> {
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

fn build_full_dataset(seed_bytes: &[u8]) -> anyhow::Result<Arc<randomx::Dataset>> {
    randomx::Dataset::new(seed_bytes)
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
    pu_plan: Vec<usize>,
    stats: Arc<MiningStats>,
    #[allow(dead_code)]
    enabled: AtomicBool,
}

impl Miner {
    pub fn new(thread_count: u32, pu_plan: Vec<usize>, stats: Arc<MiningStats>) -> Self {
        Miner {
            thread_count,
            pu_plan,
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
            let pin_pu = self.pu_plan.get(worker_id as usize).copied();
            let dataset_cache = dataset_cache.clone();
            let job_rx_clone = job_rx.clone();
            let rt_handle = tokio::runtime::Handle::current();

            thread::spawn(move || {
                worker_loop_sync(
                    worker_id,
                    pin_pu,
                    job_rx_clone,
                    rt_handle,
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
    pin_pu: Option<usize>,
    mut job_rx: watch::Receiver<Option<Job>>,
    rt_handle: tokio::runtime::Handle,
    submit_tx: mpsc::Sender<MinedShare>,
    stats: Arc<MiningStats>,
    thread_count: u32,
    dataset_cache: Arc<RxDatasetCache>,
) {
    pin_current_thread(pin_pu);
    debug!("Worker #{} started", worker_id);

    let mut current_seed: Option<String> = None;
    let mut vm: Option<randomx::Vm> = None;
    let mut current_job: Option<Job> = None;
    let mut nonce: u32 = worker_id;
    const PIPELINE_SIZE: usize = 16;
    let mut blobs: Vec<Vec<u8>> = (0..PIPELINE_SIZE).map(|_| vec![0u8; 0]).collect();

    loop {
        let latest = job_rx.borrow_and_update().clone();
        if latest.as_ref().map(|j| &j.job_id) != current_job.as_ref().map(|j| &j.job_id) {
            current_job = latest;
            nonce = worker_id;
            if let Some(job) = &current_job {
                if job.is_nicehash() {
                    debug!("Worker #{} using NiceHash nonce mask", worker_id);
                }
                let template = job.blob_bytes();
                for b in &mut blobs {
                    b.clear();
                    b.extend_from_slice(template);
                }
            }
        }

        if current_job.is_none() {
            if rt_handle.block_on(job_rx.changed()).is_err() {
                break;
            }
            continue;
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
                &mut blobs,
            );
        }
    }
}

fn create_randomx_vm(
    seed: &str,
    seed_bytes: &[u8],
    dataset_cache: &RxDatasetCache,
) -> anyhow::Result<randomx::Vm> {
    let dataset = dataset_cache
        .get_or_build(seed, seed_bytes)
        .ok_or_else(|| anyhow::anyhow!("failed to initialize RandomX dataset"))?;
    randomx::Vm::new(dataset)
}

fn mine_one_batch(
    _worker_id: u32,
    vm: &randomx::Vm,
    job: &Job,
    submit_tx: &mpsc::Sender<MinedShare>,
    stats: &MiningStats,
    thread_count: u32,
    nonce: &mut u32,
    blobs: &mut [Vec<u8>],
) {
    const BATCH_SIZE: u32 = 1024;
    const PIPELINE_SIZE: usize = 16;
    let nonce_offset = job::NONCE_OFFSET;
    let nonce_mask = job.nonce_mask();
    let target_diff = job.difficulty();
    let mut remaining = BATCH_SIZE;

    while remaining > 0 {
        let count = (remaining as usize).min(PIPELINE_SIZE);
        let mut nonces = [0u32; PIPELINE_SIZE];

        for i in 0..count {
            if *nonce > nonce_mask {
                thread::sleep(Duration::from_millis(10));
                return;
            }

            if nonce_offset + 4 <= blobs[i].len() {
                if let Some(full_nonce) = job::write_nonce(&mut blobs[i], *nonce, nonce_mask) {
                    nonces[i] = full_nonce;
                }
            }
            *nonce = (*nonce).wrapping_add(thread_count);
        }

        let mut hashes = [[0u8; randomx::HASH_SIZE]; PIPELINE_SIZE];
        if count == PIPELINE_SIZE {
            let inputs: [&[u8]; PIPELINE_SIZE] = std::array::from_fn(|i| blobs[i].as_slice());
            vm.hash_batch(inputs, &mut hashes);
            stats.record_hashes(PIPELINE_SIZE as u64);
        } else {
            for i in 0..count {
                vm.hash_one(&blobs[i], &mut hashes[i]);
            }
            stats.record_hashes(count as u64);
        }

        for i in 0..count {
            if check_hash_difficulty(&hashes[i], target_diff) {
                let share = MinedShare {
                    job_id: job.job_id.clone(),
                    nonce: job::format_nonce(nonces[i]),
                    result: hex::encode(hashes[i]),
                };
                let _ = submit_tx.try_send(share);
            }
        }
        remaining -= count as u32;
    }
}

pub fn run_benchmark(
    thread_count: u32,
    seconds: u64,
    pu_plan: Option<&[usize]>,
) -> anyhow::Result<u64> {
    let thread_count = thread_count.max(1);
    let seconds = seconds.max(1);
    let seed_bytes = [0u8; 32];
    let dataset = randomx::Dataset::new(&seed_bytes)?;

    info!(
        "benchmark starting: threads={} duration={}s flags=0x{:x}",
        thread_count,
        seconds,
        randomx::recommended_flags()
    );

    let stop = Arc::new(AtomicBool::new(false));
    let start = Arc::new(AtomicBool::new(false));
    let ready_workers = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let total_hashes = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut handles = Vec::with_capacity(thread_count as usize);

    for worker_id in 0..thread_count {
        let stop = stop.clone();
        let start = start.clone();
        let ready_workers = ready_workers.clone();
        let total_hashes = total_hashes.clone();
        let dataset = dataset.clone();

        let pin_pu = pu_plan.and_then(|v| v.get(worker_id as usize)).copied();
        handles.push(thread::spawn(move || {
            pin_current_thread(pin_pu);
            let vm = match randomx::Vm::new(dataset) {
                Ok(vm) => vm,
                Err(e) => {
                    error!("benchmark worker #{} failed to create VM: {}", worker_id, e);
                    return;
                }
            };

            ready_workers.fetch_add(1, Ordering::Release);
            while !start.load(Ordering::Acquire) && !stop.load(Ordering::Acquire) {
                thread::yield_now();
            }

            let mut nonce = worker_id;
            let mut blobs = vec![vec![0u8; 76]; 16];
            while !stop.load(Ordering::Relaxed) {
                for blob in &mut blobs {
                    blob[job::NONCE_OFFSET..job::NONCE_OFFSET + 4]
                        .copy_from_slice(&nonce.to_le_bytes());
                    nonce = nonce.wrapping_add(thread_count);
                }
                let inputs: [&[u8]; 16] = std::array::from_fn(|i| blobs[i].as_slice());
                let mut out = [[0u8; randomx::HASH_SIZE]; 16];
                vm.hash_batch(inputs, &mut out);
                total_hashes.fetch_add(16, Ordering::Relaxed);
            }
        }));
    }

    while ready_workers.load(Ordering::Acquire) < thread_count as u64 {
        thread::sleep(Duration::from_millis(10));
    }

    let started = Instant::now();
    start.store(true, Ordering::Release);
    thread::sleep(Duration::from_secs(seconds));
    stop.store(true, Ordering::Release);
    for handle in handles {
        let _ = handle.join();
    }

    let elapsed = started.elapsed().as_secs_f64();
    let hashes = total_hashes.load(Ordering::Relaxed);
    let hashrate = (hashes as f64 / elapsed) as u64;
    info!(
        "benchmark result: {} hashes in {:.2}s = {}",
        hashes,
        elapsed,
        crate::stats::format_hashrate(hashrate)
    );
    println!(
        "benchmark result: {} hashes in {:.2}s = {}",
        hashes,
        elapsed,
        crate::stats::format_hashrate(hashrate)
    );
    Ok(hashrate)
}

fn check_hash_difficulty(hash: &[u8], target_difficulty: u64) -> bool {
    if hash.len() < 32 || target_difficulty == 0 {
        return false;
    }

    let hash_val = u64::from_le_bytes([
        hash[24], hash[25], hash[26], hash[27], hash[28], hash[29], hash[30], hash[31],
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
        let mut hash = [0xff; 32];
        hash[24..32].copy_from_slice(&(target - 1).to_le_bytes());

        assert!(check_hash_difficulty(&hash, difficulty));
    }

    #[test]
    fn difficulty_check_rejects_hashes_at_or_above_target() {
        let difficulty = 2;
        let target = u64::MAX / difficulty;
        let mut hash = [0; 32];
        hash[24..32].copy_from_slice(&target.to_le_bytes());

        assert!(!check_hash_difficulty(&hash, difficulty));
    }

    #[test]
    fn difficulty_check_uses_high_64_bits_of_hash() {
        let difficulty = 2;
        let target = u64::MAX / difficulty;
        let mut hash = [0; 32];
        hash[0..8].copy_from_slice(&(target - 1).to_le_bytes());
        hash[24..32].copy_from_slice(&target.to_le_bytes());

        assert!(!check_hash_difficulty(&hash, difficulty));
    }
}
