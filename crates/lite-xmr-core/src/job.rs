//! 挖矿任务和提交结果类型。

use serde::{Deserialize, Serialize};

/// 从矿池接收的挖矿任务。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    /// 任务 ID
    pub job_id: String,

    /// 区块 blob (十六进制)
    pub blob: String,

    /// 目标难度 (十六进制)
    pub target: String,

    /// 算法标识
    pub algo: String,

    /// 区块高度
    pub height: Option<u64>,

    /// 种子哈希 (RandomX 使用)
    pub seed_hash: Option<String>,

    /// Nonce 偏移量 (在 blob 中的位置)
    pub nonce_offset: usize,
}

impl Job {
    /// 从矿池的 mining.notify 消息解析 Job。
    pub fn from_notify(params: &[serde_json::Value]) -> Option<Self> {
        if params.is_empty() {
            return None;
        }

        let job_id = params.get(0)?.as_str()?.to_string();
        let blob = params.get(1)?.as_str()?.to_string();
        let target = params.get(3)?.as_str()?.to_string();
        let algo = params
            .get(4)
            .and_then(|v| v.as_str())
            .unwrap_or("rx/0")
            .to_string();

        // Nonce 在 blob 中的偏移量: 跳过版本(2字节) + prev_hash(32字节) = 39 字节处
        // blob 是十六进制编码，所以 nonce_offset = 39 * 2 = 78
        let nonce_offset = 78;

        let seed_hash = params.get(7).and_then(|v| v.as_str()).map(|s| s.to_string());
        let height = params.get(5).and_then(|v| v.as_u64());

        Some(Job {
            job_id,
            blob,
            target,
            algo,
            height,
            seed_hash,
            nonce_offset,
        })
    }

    /// 获取 blob 的字节数组。
    pub fn blob_bytes(&self) -> Vec<u8> {
        hex::decode(&self.blob).unwrap_or_default()
    }

    /// 获取目标难度的数值。
    pub fn target_difficulty(&self) -> u64 {
        decode_difficulty(&self.target)
    }
}

/// 提交给矿池的结果。
#[derive(Debug, Clone, Serialize)]
pub struct SubmitResult {
    /// 任务 ID
    pub job_id: String,

    /// Nonce (十六进制)
    pub nonce: String,

    /// 结果哈希 (十六进制)
    pub result: String,
}

impl SubmitResult {
    /// 创建新的提交结果。
    pub fn new(job_id: &str, nonce: u32, result: &[u8]) -> Self {
        SubmitResult {
            job_id: job_id.to_string(),
            nonce: format!("{:08x}", nonce),
            result: hex::encode(result),
        }
    }
}

/// 解码矿池的难度目标为数值。
///
/// 难度目标是一个 32 字节的小端整数，转换为难度值。
fn decode_difficulty(target: &str) -> u64 {
    let target_bytes = hex::decode(target).unwrap_or_default();
    if target_bytes.len() < 4 {
        return 1;
    }

    // 取前 4 字节作为难度 (小端)
    let diff = u32::from_le_bytes([
        target_bytes[0],
        target_bytes[1],
        target_bytes[2],
        target_bytes[3],
    ]);

    if diff == 0 {
        return u64::MAX;
    }

    // 难度 = 2^256 / target
    // 简化: 使用前 4 字节估算
    u64::MAX / diff as u64
}
