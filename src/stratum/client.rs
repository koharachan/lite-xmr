use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::job::Job;

use super::transport::StratumTransport;

pub const APP_USER_AGENT: &str = "lite-xmr/0.1.0";

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
                Ok(()) => { info!("矿池连接已关闭"); break; }
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

        // ── 握手：subscribe + authorize 背靠背发送（与 XMRig 行为一致）──
        let subscribe_id = self.next_id();
        let sub = format!(
            "{{\"id\":{},\"jsonrpc\":\"2.0\",\"method\":\"mining.subscribe\",\"params\":[\"{}\"]}}\n",
            subscribe_id, APP_USER_AGENT
        );
        let auth_id = self.next_id();
        let auth = build_authorize(&self.user, &self.pass, auth_id);

        writer.write_all(sub.as_bytes()).await?;
        writer.write_all(auth.as_bytes()).await?;
        writer.flush().await?;

        debug!("handshake: sub(id={}) + auth(id={}) sent", subscribe_id, auth_id);

        // 等待 subscribe + authorize 两个响应都返回
        let mut sub_done = false;
        let mut auth_done = false;

        while !sub_done || !auth_done {
            let line = lines
                .next_line()
                .await?
                .ok_or(Error::Stratum("矿池在握手中关闭了连接".into()))?;
            let msg: serde_json::Value = serde_json::from_str(&line)?;
            let mid = msg.get("id").and_then(|v| v.as_u64());

            if mid == Some(subscribe_id) {
                if let Some(e) = get_json_error(&msg) {
                    return Err(Error::Stratum(format!("subscribe 失败: {}", e)));
                }
                sub_done = true;
                debug!("handshake: subscribe ok");
                continue;
            }

            if mid == Some(auth_id) {
                if let Some(e) = get_json_error(&msg) {
                    return Err(Error::Stratum(format!("授权失败: {}", e)));
                }
                auth_done = true;
                info!("矿池授权成功");
                let _ = event_tx.send(StratumEvent::Connected).await;
                continue;
            }

            // 握手期间矿池推送的消息（mining.notify / set_extranonce 等）
            self.handle_incoming(&msg, job_tx, event_tx, keepalive).await?;
        }

        // ── 主循环 ──
        if keepalive {
            info!("保活模式: 保持连接不挖矿");
            keepalive_loop(&mut lines, shutdown_rx).await
        } else {
            mining_loop(&mut lines, shutdown_rx, submit_rx, &mut writer, &self.user, self, job_tx, event_tx).await
        }
    }

    async fn handle_incoming(
        &self,
        msg: &serde_json::Value,
        job_tx: &mut watch::Sender<Option<Job>>,
        event_tx: &mut mpsc::Sender<StratumEvent>,
        keepalive: bool,
    ) -> Result<()> {
        if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
            match method {
                "mining.notify" => {
                    if keepalive { return Ok(()); }
                    if let Some(params) = msg.get("params").and_then(|p| p.as_array()) {
                        if let Some(job) = Job::from_notify(params) {
                            debug!("job: id={} height={:?} algo={}", job.job_id, job.height, job.algo);
                            let _ = job_tx.send(Some(job.clone())).ok();
                            let _ = event_tx.send(StratumEvent::NewJob(job)).await;
                        }
                    }
                }
                "mining.set_target" | "mining.set_extranonce" | "mining.set_difficulty" => {
                    debug!("{}", method);
                }
                _ => debug!("unknown method: {}", method),
            }
            return Ok(());
        }

        if msg.get("id").is_some() {
            match msg.get("error") {
                None | Some(serde_json::Value::Null) => {
                    let _ = event_tx.send(StratumEvent::Accepted).await;
                }
                Some(err) => {
                    let _ = event_tx.send(StratumEvent::Rejected(format_json_error(err))).await;
                }
            }
        }

        Ok(())
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
    }

    fn next_id(&self) -> u64 {
        self.request_id.fetch_add(1, Ordering::Relaxed)
    }
}

// ══════════════════════════════════════════

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
                            Err(e) => { warn!("JSON parse error: {} (line: {})", e, &line[..line.len().min(120)]); continue; }
                        };
                        client.handle_incoming(&msg, job_tx, event_tx, false).await?;
                    }
                    Ok(None) => { info!("矿池关闭了连接"); break; }
                    Err(e) => return Err(Error::Network(e.to_string())),
                }
            }
            Some((job_id, nonce, result)) = submit_rx.recv() => {
                let id = client.next_id();
                let m = format!(
                    "{{\"id\":{},\"jsonrpc\":\"2.0\",\"method\":\"mining.submit\",\"params\":[\"{}\",\"{}\",\"{}\",\"{}\"]}}\n",
                    id, user, job_id, nonce, result
                );
                if let Err(e) = writer.write_all(m.as_bytes()).await {
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
            _ = shutdown_rx.changed() => { info!("收到关闭信号"); break; }
            line = lines.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        let l = l.trim().to_string();
                        if l.is_empty() { continue; }
                        if let Ok(msg) = serde_json::from_str::<serde_json::Value>(&l) {
                            debug!("keepalive: id={:?} method={:?}", msg.get("id"), msg.get("method"));
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

// ══════════════════════════════════════════

fn build_authorize(user: &str, pass: &str, id: u64) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(256);
    write!(s, "{{\"id\":{},\"jsonrpc\":\"2.0\",\"method\":\"mining.authorize\",\"params\":[\"{}\",\"{}\"", id, user, pass).unwrap();
    for algo in SUPPORTED_ALGOS {
        write!(s, ",\"{}\"", algo).unwrap();
    }
    s.push_str("]}\n");
    s
}

fn get_json_error(msg: &serde_json::Value) -> Option<String> {
    let err = msg.get("error")?;
    if err.is_null() { return None; }
    if let Some(arr) = err.as_array() {
        if arr.len() > 1 { if let Some(s) = arr.get(1).and_then(|v| v.as_str()) { return Some(s.to_string()); } }
        if let Some(s) = arr.first().and_then(|v| v.as_str()) { return Some(s.to_string()); }
    }
    if let Some(s) = err.get("message").and_then(|m| m.as_str()) { return Some(s.to_string()); }
    err.as_str().map(|s| s.to_string())
}

fn format_json_error(err: &serde_json::Value) -> String {
    if let Some(arr) = err.as_array() {
        return arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(": ");
    }
    err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown").to_string()
}
