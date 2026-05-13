use serde::{Deserialize, Serialize};

/// Monero 区块头中 nonce 字段的固定字节偏移量（从 0 开始）。
/// 参考: cryptonote_format_utils.cpp, block_header struct layout:
///   major_version(1) + minor_version(1) + timestamp(8) + prev_id(32) + nonce(4) = 46
///   但实际模板中 nonce 起始于偏移 39（不含保留字段）。
pub const NONCE_OFFSET: usize = 39;

fn default_difficulty() -> u64 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub job_id: String,
    pub blob: String,
    pub target: String,
    pub algo: String,
    pub height: Option<u64>,
    pub seed_hash: Option<String>,

    #[serde(skip, default = "Vec::new")]
    blob_bytes: Vec<u8>,

    #[serde(skip, default = "default_difficulty")]
    difficulty: u64,
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum JobParseError {
    MissingField(&'static str),
    InvalidHex(&'static str),
    InvalidTarget(String),
}

impl Job {
    const IDX_JOB_ID: usize = 0;
    const IDX_BLOB: usize = 1;
    const IDX_TARGET: usize = 2;
    const IDX_ALGO: usize = 3;
    const IDX_HEIGHT: usize = 4;
    const IDX_SEED_HASH: usize = 5;

    pub fn from_notify(params: &[serde_json::Value]) -> Option<Self> {
        if params.len() < 4 {
            tracing::warn!("mining.notify: params too short ({})", params.len());
            return None;
        }

        let job_id = match params.get(Self::IDX_JOB_ID).and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                tracing::warn!("mining.notify: missing job_id");
                return None;
            }
        };

        let blob = match params.get(Self::IDX_BLOB).and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                tracing::warn!("mining.notify: missing blob");
                return None;
            }
        };

        let target = match params.get(Self::IDX_TARGET).and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                tracing::warn!("mining.notify: missing target");
                return None;
            }
        };

        let algo = params
            .get(Self::IDX_ALGO)
            .and_then(|v| v.as_str())
            .unwrap_or("rx/0")
            .to_string();

        let height = params.get(Self::IDX_HEIGHT).and_then(|v| v.as_u64());
        let seed_hash = params
            .get(Self::IDX_SEED_HASH)
            .and_then(|v| v.as_str())
            .map(String::from);

        let blob_bytes = match hex::decode(&blob) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("mining.notify: invalid hex in blob: {}", e);
                return None;
            }
        };

        let difficulty = match decode_target(&target) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("mining.notify: {}", e);
                return None;
            }
        };

        Some(Job {
            job_id,
            blob,
            target,
            algo,
            height,
            seed_hash,
            blob_bytes,
            difficulty,
        })
    }

    pub fn blob_bytes(&self) -> &[u8] {
        &self.blob_bytes
    }

    pub fn difficulty(&self) -> u64 {
        self.difficulty
    }
}

#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct SubmitResult {
    pub job_id: String,
    pub nonce: String,
    pub result: String,
}

impl SubmitResult {
    /// nonce 采用小写十六进制、宽度 8，即原样填入区块头的 nonce 字节。
    #[allow(dead_code)]
    pub fn new(job_id: &str, nonce: u32, result: &[u8]) -> Self {
        SubmitResult {
            job_id: job_id.to_string(),
            nonce: format!("{:08x}", nonce),
            result: hex::encode(result),
        }
    }
}

/// 将 Monero stratum 协议中的 target 字段转换为难度值。
///
/// target 格式：大端十六进制字符串，长度 8（4 字节）或 16（8 字节）。
/// 难度 = MAX_u64 / target_64 或 MAX_u32 / target_32。
/// target 为 0 时返回 u64::MAX。
fn decode_target(target_hex: &str) -> Result<u64, String> {
    let bytes =
        hex::decode(target_hex).map_err(|e| format!("invalid hex in target '{}': {}", target_hex, e))?;

    let diff = match bytes.len() {
        4 => {
            let mut b = [0u8; 4];
            b.copy_from_slice(&bytes);
            u32::from_be_bytes(b) as u64
        }
        8 => {
            let mut b = [0u8; 8];
            b.copy_from_slice(&bytes);
            u64::from_be_bytes(b)
        }
        n => return Err(format!("unsupported target length: {} bytes", n)),
    };

    if diff == 0 {
        return Ok(u64::MAX);
    }

    let max_val = if bytes.len() == 4 {
        u32::MAX as u64
    } else {
        u64::MAX
    };

    Ok(max_val / diff)
}
