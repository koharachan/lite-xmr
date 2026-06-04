use std::collections::BTreeMap;

pub const RX0: &str = "rx/0";

#[allow(dead_code)]
pub const CPU_AUTO_ALGOS: &[&str] = &[
    "cn/1",
    "cn/2",
    "cn/r",
    "cn/fast",
    "cn/half",
    "cn/xao",
    "cn/rto",
    "cn/rwz",
    "cn/zls",
    "cn/double",
    "cn/ccx",
    "cn-lite/1",
    "cn-heavy/xhv",
    "cn-pico",
    "cn-pico/tlo",
    "cn/upx2",
    "rx/0",
    "rx/wow",
    "rx/arq",
    "rx/graft",
    "rx/sfx",
    "rx/keva",
    "argon2/chukwa",
    "argon2/chukwav2",
    "argon2/ninja",
    "astrobwt",
];

pub const SUPPORTED_ALGOS: &[&str] = &[RX0];

pub fn is_supported(algo: &str) -> bool {
    matches!(normalize(algo), "rx/0")
}

pub fn normalize(algo: &str) -> &str {
    match algo {
        "randomx" | "randomx/0" => "rx/0",
        other => other,
    }
}

pub fn filtered_algo_perf(configured: &BTreeMap<String, f64>) -> BTreeMap<String, f64> {
    configured
        .iter()
        .filter_map(|(algo, perf)| {
            let normalized = normalize(algo);
            (is_supported(normalized) && *perf > 0.0).then(|| (normalized.to_string(), *perf))
        })
        .collect()
}
