//! 挖矿统计信息。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

/// 挖矿统计信息收集器。
pub struct MiningStats {
    /// 开始时间
    start_time: Instant,

    /// 已接受份额数
    accepted: AtomicU64,

    /// 已拒绝份额数
    rejected: AtomicU64,

    /// 总哈希次数 (用于计算算力)
    total_hashes: AtomicU64,

    /// 上次算力计算时间和哈希数 (需要 Mutex 保护)
    last_sample: Mutex<(Instant, u64)>,

    /// 当前算力 (hashes/s)
    current_hashrate: AtomicU64,
}

impl MiningStats {
    /// 创建新的统计收集器。
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

    /// 记录一次哈希计算。
    pub fn record_hashes(&self, count: u64) {
        self.total_hashes.fetch_add(count, Ordering::Relaxed);
        let now = Instant::now();

        // 每秒更新一次算力显示
        let mut sample = self.last_sample.lock().unwrap();
        let elapsed = now.duration_since(sample.0).as_secs_f64();
        if elapsed >= 1.0 {
            let current = self.total_hashes.load(Ordering::Relaxed);
            let hashrate = ((current - sample.1) as f64 / elapsed) as u64;
            self.current_hashrate.store(hashrate, Ordering::Relaxed);
            *sample = (now, current);
        }
    }

    /// 记录接受的份额。
    pub fn record_accepted(&self) {
        self.accepted.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录拒绝的份额。
    pub fn record_rejected(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }

    /// 获取当前算力 (hashes/s)。
    pub fn hashrate(&self) -> u64 {
        self.current_hashrate.load(Ordering::Relaxed)
    }

    /// 获取已接受份额数。
    pub fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::Relaxed)
    }

    /// 获取已拒绝份额数。
    pub fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }

    /// 获取总哈希次数。
    pub fn total_hashes(&self) -> u64 {
        self.total_hashes.load(Ordering::Relaxed)
    }

    /// 获取运行时间 (秒)。
    pub fn uptime_secs(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    /// 格式化算力为人类可读字符串。
    pub fn format_hashrate(&self) -> String {
        format_hashrate(self.hashrate())
    }
}

/// 格式化算力值。
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
