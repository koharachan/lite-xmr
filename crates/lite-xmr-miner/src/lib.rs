//! lite-xmr-miner: 挖矿核心实现。
//!
//! 本 crate 实现挖矿核心逻辑：
//! - 使用 `randomx-rs` 进行 RandomX 哈希计算
//! - 管理多个挖矿 worker 线程
//! - 处理任务分发和结果收集

pub mod worker;

pub use worker::MiningPool;
