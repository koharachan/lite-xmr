use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::job::Job;

use super::transport::StratumTransport;

pub const APP_USER_AGENT: &str = "lite-xmr/0.1.0";

/// 支持的算法列表（按优先级）。机枪池会从此列表中挑选第一个匹配的算法。
const SUPPORTED_ALGOS: &[&str] = &["rx/0"];

#[derive(Debug, Clone)]
pub enum StratumEvent {
    NewJob(Job),
    Accepted,
    Rejected(String),
    Connected,
    Disconnected(String),
}

pub struct StratumClient {
    url: String,
    user: String,
    pass: String,
    use_tls: bool,
    running: Arc<AtomicBool>,
    request_id: AtomicU64,
}

impl StratumClient {
    pub fn new(url: String, user: String, pass: String, use_tls: bool) -> Self {
        StratumClient {
            url,
            user,
            pass,
            use_tls,
            running: Arc::new(AtomicBool::new(false)),
            request_id: AtomicU64::new(1),
        }
    }

    pub async fn run(
        &self,
        mut job_tx: watch::Sender<Option<Job>>,
        mut submit_rx: mpsc::Receiver<(String, String, String)>,
        mut event_tx: mpsc::Sender<StratumEvent>,
        keepalive: bool,
    ) -> Result<()> {
        self.running.store(true, Ordering::Release);

        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let running = self.running.clone();
        tokio::spawn(async move {
            while running.load(Ordering::Acquire) {
                tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
            }
            let _ = shutdown_tx.send(true);
        });

        while self.running.load(Ordering::Acquire) {
            info!("正在连接矿池 {} ...", self.url);

            match self
                .run_session(&mut job_tx, &mut submit_rx, &mut event_tx, &mut shutdown_rx, keepalive)
                .await
            {
                Ok(()) => {
                    info!("矿池连接已关闭");
                    break;
                }
                Err(e) => {
                    warn!("矿池连接错误: {}, 5秒后重连...", e);
                    let _ = event_tx.send(StratumEvent::Disconnected(e.to_string())).await;
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
        }

        self.running.store(false, Ordering::Release);
        Ok(())
    }

    async fn run_session(
        &self,
        job_tx: &mut watch::Sender<Option<Job>>,
        submit_rx: &mut mpsc::Receiver<(String, String, String)>,
        event_tx: &mut mpsc::Sender<StratumEvent>,
        shutdown_rx: &mut watch::Receiver<bool>,
        keepalive: bool,
    ) -> Result<()> {
        let transport = StratumTransport::connect(&self.url, self.use_tls)
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        let (reader, mut writer) = tokio::io::split(transport);
        let mut lines = BufReader::new(reader).lines();

        // === 阶段 1: mining.subscribe ===
        let subscribe_id = self.next_id();
        let subscribe_msg = format!(
            "{{\"id\":{},\"jsonrpc\":\"2.0\",\"method\":\"mining.subscribe\",\"params\":[\"{}\"]}}\n",
            subscribe_id, APP_USER_AGENT
        );
        writer.write_all(subscribe_msg.as_bytes()).await?;
        debug!("subscribe(id={})", subscribe_id);

        let mut extranonce1 = String::new();
        let mut extranonce2_size: u32 = 4;

        loop {
            let line = lines
                .next_line()
                .await?
                .ok_or(Error::Stratum("矿池在订阅阶段关闭了连接".into()))?;

            let msg: serde_json::Value = serde_json::from_str(&line)?;

            if is_response_id(&msg, subscribe_id) {
                debug!("subscribe response: {}", line.trim());
                if let Some(err) = get_json_error(&msg) {
                    return Err(Error::Stratum(format!("订阅失败: {}", err)));
                }
                // 解析 extranonce（EthereumStratum 格式）
                if let Some(result) = msg.get("result").and_then(|r| r.as_array()) {
                    // result[0] = [subscription_details...]
                    // result[1] = extranonce1 (hex)
                    // result[2] = extranonce2_size
                    if let Some(en1) = result.get(1).and_then(|v| v.as_str()) {
                        extranonce1 = en1.to_string();
                    }
                    if let Some(sz) = result.get(2).and_then(|v| v.as_u64()) {
                        extranonce2_size = sz as u32;
                    }
                }
                debug!(
                    "extranonce1={}, extranonce2_size={}",
                    extranonce1, extranonce2_size
                );
                break;
            }

            self.handle_incoming(&msg, job_tx, event_tx, keepalive).await?;
        }

        // === 阶段 2: mining.authorize ===
        // XMRig 标准: 只传 wallet + password。但 c3pool 机枪池需要在 params 中声明算法。
        let auth_id = self.next_id();
        let auth_msg = build_authorize(&self.user, &self.pass, auth_id);
        writer.write_all(auth_msg.as_bytes()).await?;
        debug!("authorize(id={})", auth_id);

        loop {
            let line = lines
                .next_line()
                .await?
                .ok_or(Error::Stratum("矿池在授权阶段关闭了连接".into()))?;

            let msg: serde_json::Value = serde_json::from_str(&line)?;

            if is_response_id(&msg, auth_id) {
                debug!("authorize response: {}", line.trim());

                if let Some(err) = get_json_error(&msg) {
                    return Err(Error::Stratum(format!("授权失败: {}", err)));
                }

                // 成功：result 为 true 或非 null
                if msg.get("result").and_then(|r| r.as_bool()).unwrap_or(false)
                    || (msg.get("result").is_some() && msg.get("error").is_none())
                {
                    info!("矿池授权成功");
                    let _ = event_tx.send(StratumEvent::Connected).await;
                    break;
                }

                return Err(Error::Stratum("授权响应格式无法识别".into()));
            }

            self.handle_incoming(&msg, job_tx, event_tx, keepalive).await?;
        }

        // === 阶段 3: 主循环 ===
        if keepalive {
            info!("保活模式: 保持连接不挖矿");
            keepalive_loop(&mut lines, shutdown_rx).await?;
        } else {
            mining_loop(&mut lines, shutdown_rx, submit_rx, &mut writer, &self.user, self, job_tx, event_tx).await?;
        }

        Ok(())
    }

    async fn handle_incoming(
        &self,
        msg: &serde_json::Value,
        job_tx: &mut watch::Sender<Option<Job>>,
        event_tx: &mut mpsc::Sender<StratumEvent>,
        keepalive: bool,
    ) -> Result<()> {
        // 推送消息（带 method）
        if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
            match method {
                "mining.notify" => {
                    if keepalive {
                        debug!("忽略 mining.notify (保活模式)");
                        return Ok(());
                    }
                    if let Some(params) = msg.get("params").and_then(|p| p.as_array()) {
                        if let Some(job) = Job::from_notify(params) {
                            debug!(
                                "新任务: job_id={}, height={:?}, algo={}",
                                job.job_id, job.height, job.algo
                            );
                            let _ = job_tx.send(Some(job.clone())).ok();
                            let _ = event_tx.send(StratumEvent::NewJob(job)).await;
                        }
                    }
                }
                "mining.set_target" => debug!("mining.set_target"),
                "mining.set_extranonce" => debug!("mining.set_extranonce"),
                "mining.set_difficulty" => debug!("mining.set_difficulty"),
                _ => debug!("未知方法: {}", method),
            }
            return Ok(());
        }

        // 响应消息（带 id）
        if msg.get("id").is_some() {
            match msg.get("error") {
                None | Some(serde_json::Value::Null) => {
                    let _ = event_tx.send(StratumEvent::Accepted).await;
                }
                Some(err) => {
                    let reason = format_json_error(err);
                    let _ = event_tx
                        .send(StratumEvent::Rejected(reason))
                        .await;
                }
            }
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
    }

    fn next_id(&self) -> u64 {
        self.request_id.fetch_add(1, Ordering::Relaxed)
    }
}

// ═══════════════════════════════════════════════════════════════
// 主循环
// ═══════════════════════════════════════════════════════════════

async fn mining_loop(
    lines: &mut tokio::io::Lines<BufReader<tokio::io::ReadHalf<StratumTransport>>>,
    shutdown_rx: &mut watch::Receiver<bool>,
    submit_rx: &mut mpsc::Receiver<(String, String, String)>,
    writer: &mut tokio::io::WriteHalf<StratumTransport>,
    user: &str,
    client: &StratumClient,
    job_tx: &mut watch::Sender<Option<Job>>,
    event_tx: &mut mpsc::Sender<StratumEvent>,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                info!("收到关闭信号，断开连接");
                break;
            }
            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        let line = line.trim().to_string();
                        if line.is_empty() { continue; }
                        let msg: serde_json::Value = match serde_json::from_str(&line) {
                            Ok(m) => m,
                            Err(e) => {
                                warn!("JSON 解析错误: {} (line: {})", e, &line[..line.len().min(120)]);
                                continue;
                            }
                        };
                        client.handle_incoming(&msg, job_tx, event_tx, false).await?;
                    }
                    Ok(None) => { info!("矿池关闭了连接"); break; }
                    Err(e) => return Err(Error::Network(e.to_string())),
                }
            }
            Some((job_id, nonce, result)) = submit_rx.recv() => {
                let id = client.next_id();
                let submit_msg = format!(
                    "{{\"id\":{},\"jsonrpc\":\"2.0\",\"method\":\"mining.submit\",\"params\":[\"{}\",\"{}\",\"{}\",\"{}\"]}}\n",
                    id, user, job_id, nonce, result
                );
                if let Err(e) = writer.write_all(submit_msg.as_bytes()).await {
                    return Err(Error::Network(e.to_string()));
                }
            }
        }
    }
    Ok(())
}

async fn keepalive_loop(
    lines: &mut tokio::io::Lines<BufReader<tokio::io::ReadHalf<StratumTransport>>>,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                info!("收到关闭信号，断开连接");
                break;
            }
            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        let line = line.trim().to_string();
                        if line.is_empty() { continue; }
                        let msg: serde_json::Value = match serde_json::from_str(&line) {
                            Ok(m) => m, Err(_) => continue,
                        };
                        if msg.get("method").is_some() {
                            debug!("保活: 忽略 method={:?}", msg.get("method"));
                        } else if msg.get("id").is_some() {
                            debug!("保活: 收到响应 id={:?}", msg.get("id"));
                        }
                    }
                    Ok(None) => { info!("矿池关闭了连接"); break; }
                    Err(e) => return Err(Error::Network(e.to_string())),
                }
            }
        }
    }
    Ok(())
}

// ═══════════════════════════════════════════════════════════════
// 辅助
// ═══════════════════════════════════════════════════════════════

fn build_authorize(user: &str, pass: &str, id: u64) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(256);
    // jsonrpc 2.0 + 算法列表（兼容 c3pool 机枪池）
    write!(
        s,
        "{{\"id\":{},\"jsonrpc\":\"2.0\",\"method\":\"mining.authorize\",\"params\":[\"{}\",\"{}\"",
        id, user, pass
    )
    .unwrap();
    for algo in SUPPORTED_ALGOS {
        write!(s, ",\"{}\"", algo).unwrap();
    }
    s.push_str("]}\n");
    s
}

fn is_response_id(msg: &serde_json::Value, expected: u64) -> bool {
    msg.get("id").and_then(|v| v.as_u64()) == Some(expected)
}

/// EthereumStratum / JSON-RPC 2.0 兼容的错误提取：
///   1) error 是数组  → 取 error[1]（字符串消息）
///   2) error 是对象  → 取 error.message
///   3) error 是字符串 → 直接返回
fn get_json_error(msg: &serde_json::Value) -> Option<String> {
    let err = msg.get("error")?;
    if err.is_null() {
        return None;
    }
    // 数组格式: ["message", code]
    if let Some(arr) = err.as_array() {
        if arr.len() > 1 {
            if let Some(s) = arr.get(1).and_then(|v| v.as_str()) {
                return Some(s.to_string());
            }
        }
        if let Some(s) = arr.first().and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
    }
    // 对象格式: {"code": -1, "message": "..."}
    if let Some(msg) = err.get("message").and_then(|m| m.as_str()) {
        return Some(msg.to_string());
    }
    // 字符串格式
    err.as_str().map(|s| s.to_string())
}

fn format_json_error(err: &serde_json::Value) -> String {
    if let Some(arr) = err.as_array() {
        return arr
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(": ");
    }
    if let Some(msg) = err.get("message").and_then(|m| m.as_str()) {
        return msg.to_string();
    }
    err.as_str().unwrap_or("未知错误").to_string()
}
