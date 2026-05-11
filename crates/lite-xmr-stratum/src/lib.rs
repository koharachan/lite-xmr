//! lite-xmr-stratum: 异步 Stratum 协议客户端。
//!
//! 基于 tokio 实现的 Stratum 协议客户端，支持：
//! - TCP/TLS 连接
//! - SOCKS5 代理
//! - 标准 Stratum 协议 (mining.subscribe, mining.authorize, mining.submit)
//! - 自动重连和故障转移

pub mod client;
pub mod transport;

pub use client::{StratumClient, StratumEvent};
