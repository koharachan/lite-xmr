//! 统一错误类型定义。

use thiserror::Error;

/// lite-xmr 统一错误类型。
#[derive(Debug, Error)]
pub enum Error {
    #[error("配置错误: {0}")]
    Config(String),

    #[error("网络错误: {0}")]
    Network(String),

    #[error("Stratum 协议错误: {0}")]
    Stratum(String),

    #[error("I/O 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON 解析错误: {0}")]
    Json(#[from] serde_json::Error),

    #[error("TLS 错误: {0}")]
    Tls(String),

    #[error("RandomX 初始化错误: {0}")]
    RandomX(String),

    #[error("CPU 检测错误: {0}")]
    CpuInfo(String),

    #[error("信号处理错误: {0}")]
    Signal(String),
}

/// 便捷的 Result 类型别名。
pub type Result<T> = std::result::Result<T, Error>;
