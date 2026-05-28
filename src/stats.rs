use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub struct MiningStats {
    start_time: Instant,
    accepted: AtomicU64,
    rejected: AtomicU64,
    total_hashes: AtomicU64,
    last_sample: Mutex<(Instant, u64)>,
    current_hashrate: AtomicU64,
}

impl Default for MiningStats {
    fn default() -> Self {
        Self::new()
    }
}

impl MiningStats {
    pub fn new() -> Self {
        MiningStats {
            start_time: Instant::now(),
            accepted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            total_hashes: AtomicU64::new(0),
            last_sample: Mutex::new((Instant::now(), 0)),
            current_hashrate: AtomicU64::new(0),
        }
    }

    pub fn record_hashes(&self, count: u64) {
        self.total_hashes.fetch_add(count, Ordering::Relaxed);
        let now = Instant::now();

        let mut sample = self.last_sample.lock().unwrap();
        let elapsed = now.duration_since(sample.0).as_secs_f64();
        if elapsed >= 1.0 {
            let current = self.total_hashes.load(Ordering::Relaxed);
            let hashrate = ((current - sample.1) as f64 / elapsed) as u64;
            self.current_hashrate.store(hashrate, Ordering::Relaxed);
            *sample = (now, current);
        }
    }

    pub fn record_accepted(&self) {
        self.accepted.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_rejected(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub fn hashrate(&self) -> u64 {
        self.current_hashrate.load(Ordering::Relaxed)
    }

    pub fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::Relaxed)
    }

    pub fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }

    pub fn total_hashes(&self) -> u64 {
        self.total_hashes.load(Ordering::Relaxed)
    }

    pub fn uptime_secs(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    pub fn format_hashrate(&self) -> String {
        format_hashrate(self.hashrate())
    }
}

pub fn format_hashrate(hashes_per_sec: u64) -> String {
    const K: f64 = 1000.0;
    const M: f64 = 1_000_000.0;
    const H: f64 = 1_000_000_000.0;

    let h = hashes_per_sec as f64;
    if h >= H {
        format!("{:.2} GH/s", h / H)
    } else if h >= M {
        format!("{:.2} MH/s", h / M)
    } else if h >= K {
        format!("{:.2} kH/s", h / K)
    } else {
        format!("{} H/s", hashes_per_sec)
    }
}
