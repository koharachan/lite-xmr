//! lite-xmr-core: 共享类型、配置、错误定义和 CPU 信息检测。
//!
//! 本 crate 为 lite-xmr 项目提供基础设施工具，包括：
//! - 挖矿配置（`Config`）
//! - 错误类型（`Error`）
//! - CPU 拓扑检测（`CpuInfo`）
//! - Stratum 协议相关类型（`Job`、`SubmitResult` 等）
//! - 挖矿统计（`MiningStats`）

pub mod config;
pub mod cpu;
pub mod error;
pub mod job;
pub mod stats;

pub use config::Config;
pub use cpu::CpuInfo;
pub use error::{Error, Result};
pub use job::{Job, SubmitResult};
pub use stats::MiningStats;
