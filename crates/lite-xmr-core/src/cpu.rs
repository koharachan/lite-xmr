//! CPU 信息检测。
//!
//! 使用 `raw-cpuid` 和 `sysinfo` 检测 CPU 特性，用于：
//! - 自动选择挖矿线程数
//! - 确定 RandomX 模式（fast mode 需要足够内存）
//! - 显示 CPU 信息摘要

use raw_cpuid::CpuId;
use sysinfo::System;
use tracing::info;

/// CPU 信息摘要。
#[derive(Debug, Clone)]
pub struct CpuInfo {
    /// CPU 型号名称
    pub brand: String,
    /// 逻辑核心数
    pub logical_cores: usize,
    /// 物理核心数
    pub physical_cores: usize,
    /// 是否支持 AES-NI
    pub aes_ni: bool,
    /// 是否支持 AVX2
    pub avx2: bool,
    /// 是否支持 AVX512F
    pub avx512f: bool,
    /// 是否支持 SHA 扩展
    pub sha_ext: bool,
    /// L3 缓存大小 (bytes)
    pub l3_cache: u64,
    /// 总内存大小 (bytes)
    pub total_memory: u64,
}

impl CpuInfo {
    /// 检测当前系统的 CPU 信息。
    pub fn detect() -> Self {
        let cpuid = CpuId::new();
        let mut sys = System::new();
        sys.refresh_cpu();
        sys.refresh_memory();

        let logical_cores = num_cpus::get();
        let physical_cores = sys.physical_core_count().unwrap_or(logical_cores);

        // CPU 品牌
        let brand = cpuid
            .get_processor_brand_string()
            .map(|b| b.as_str().trim().to_string())
            .unwrap_or_else(|| "Unknown CPU".to_string());

        // 特性检测
        let features = cpuid.get_feature_info();
        let ext_features = cpuid.get_extended_feature_info();

        let aes_ni = features.map(|f| f.has_aesni()).unwrap_or(false);
        let avx2 = ext_features.as_ref().map(|f| f.has_avx2()).unwrap_or(false);
        let avx512f = ext_features.as_ref().map(|f| f.has_avx512f()).unwrap_or(false);
        let sha_ext = ext_features.as_ref().map(|f| f.has_sha()).unwrap_or(false);

        // 缓存信息 - 使用 DMI 或 /sys 文件系统获取
        let l3_cache = read_l3_cache_from_sys();

        let total_memory = sys.total_memory() * 1024; // KB -> bytes

        CpuInfo {
            brand,
            logical_cores,
            physical_cores,
            aes_ni,
            avx2,
            avx512f,
            sha_ext,
            l3_cache,
            total_memory,
        }
    }

    /// 打印 CPU 信息摘要。
    pub fn print_summary(&self) {
        info!("CPU 信息:");
        info!("  型号:       {}", self.brand);
        info!("  逻辑核心:   {}", self.logical_cores);
        info!("  物理核心:   {}", self.physical_cores);
        info!("  AES-NI:     {}", self.aes_ni);
        info!("  AVX2:       {}", self.avx2);
        info!("  AVX-512F:   {}", self.avx512f);
        info!("  SHA 扩展:   {}", self.sha_ext);
        info!("  L3 缓存:    {}", format_bytes(self.l3_cache));
        info!("  总内存:     {}", format_bytes(self.total_memory));
    }

    /// 推荐的挖矿线程数。
    pub fn recommended_threads(&self) -> u32 {
        (self.logical_cores.saturating_sub(1)).max(1) as u32
    }

    /// 是否可以使用 RandomX fast mode。
    pub fn can_use_fast_mode(&self) -> bool {
        self.total_memory >= 3 * 1024 * 1024 * 1024
    }
}

/// 从 /sys 文件系统读取 L3 缓存大小。
fn read_l3_cache_from_sys() -> u64 {
    // 尝试从 /sys/devices/system/cpu/cpu0/cache/index3/size 读取
    let path = "/sys/devices/system/cpu/cpu0/cache/index3/size";
    if let Ok(content) = std::fs::read_to_string(path) {
        let trimmed = content.trim();
        // 格式通常为 "8192K" 或 "16M"
        if let Some(num_str) = trimmed.strip_suffix('K') {
            if let Ok(kb) = num_str.parse::<u64>() {
                return kb * 1024;
            }
        } else if let Some(num_str) = trimmed.strip_suffix('M') {
            if let Ok(mb) = num_str.parse::<u64>() {
                return mb * 1024 * 1024;
            }
        }
    }
    0
}

/// 格式化字节数为人类可读格式。
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}
