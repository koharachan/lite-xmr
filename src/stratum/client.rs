use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::job::Job;

use super::transport::StratumTransport;

pub const APP_USER_AGENT: &str =
    "XMRig/6.26.0 (Windows NT 10.0; Win64; x64) libuv/1.51.0 msvc/2022";

/// 支持的算法列表（c3pool 从 login.algo 数组中选取匹配项）
const SUPPORTED_ALGOS: &[&str] = &[
    "rx/0", "rx/wow", "rx/arq", "rx/graft", "rx/sfx", "rx/yada",
    "cn/1", "cn/2", "cn/r", "cn/fast", "cn/half", "cn/xao",
    "cn/rto", "cn/rwz", "cn/zls", "cn/double", "cn/ccx",
    "cn-lite/1", "cn-heavy/0", "cn-heavy/tube", "cn-heavy/xhv",
    "cn-pico", "cn-pico/tlo", "cn/upx2", "rx/2",
    "argon2/chukwa", "argon2/chukwav2", "argon2/ninja",
    "ghostrider",
];

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
            url, user, pass, use_tls,
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

        // ── Login（c3pool 协议：一次握手取代 subscribe + authorize）──
        let login_id = self.next_id();
        let login_msg = build_login(&self.user, &self.pass, login_id);
        writer.write_all(login_msg.as_bytes()).await?;
        writer.flush().await?;
        debug!("login(id={})", login_id);

        // 等待 login 响应（可能包含第一个 job）
        loop {
            let line = lines.next_line().await?
                .ok_or(Error::Stratum("矿池在登录阶段关闭了连接".into()))?;
            let msg: serde_json::Value = serde_json::from_str(&line)?;
            let mid = msg.get("id").and_then(|v| v.as_u64());

            if mid == Some(login_id) {
                if let Some(e) = get_json_error(&msg) {
                    return Err(Error::Stratum(format!("login 失败: {}", e)));
                }

                // login 成功后，result 中包含第一个 job
                if let Some(result) = msg.get("result").and_then(|v| v.as_object()) {
                    if let Some(job_obj) = result.get("job") {
                        if let Some(job) = Job::from_c3pool_job(job_obj) {
                            debug!("login job: id={} height={:?} algo={}", job.job_id, job.height, job.algo);
                            let _ = job_tx.send(Some(job.clone())).ok();
                            let _ = event_tx.send(StratumEvent::NewJob(job)).await;
                        }
                    }
                }

                info!("矿池登录成功");
                let _ = event_tx.send(StratumEvent::Connected).await;
                break;
            }

            // login 响应之前的中间消息
            handle_msg(&msg, job_tx, event_tx, keepalive).await?;
        }

        // ── 主循环 ──
        if keepalive {
            info!("保活模式");
            keepalive_loop(&mut lines, shutdown_rx).await
        } else {
            mining_loop(&mut lines, shutdown_rx, submit_rx, &mut writer, &self.user, self, job_tx, event_tx).await
        }
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
    // 跟踪当前 session id（用于 submit）
    let mut session_id = String::new();

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                info!("收到关闭信号"); break;
            }
            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        let line = line.trim().to_string();
                        if line.is_empty() { continue; }
                        let msg: serde_json::Value = match serde_json::from_str(&line) {
                            Ok(m) => m,
                            Err(e) => { warn!("JSON error: {} (line: {})", e, &line[..line.len().min(120)]); continue; }
                        };
                        // 从 job 通知中提取 session_id
                        if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                            if method == "job" {
                                if let Some(p) = msg.get("params").and_then(|p| p.get("id")).and_then(|v| v.as_str()) {
                                    session_id = p.to_string();
                                }
                            }
                        }
                        handle_msg(&msg, job_tx, event_tx, false).await?;
                    }
                    Ok(None) => { info!("矿池关闭了连接"); break; }
                    Err(e) => return Err(Error::Network(e.to_string())),
                }
            }
            Some((job_id, nonce, result)) = submit_rx.recv() => {
                let id = client.next_id();
                let m = build_submit(user, &session_id, &job_id, &nonce, &result, id);
                debug!("submit: id={} session={} job={} nonce={}", id, session_id, job_id, nonce);
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
            _ = shutdown_rx.changed() => { break; }
            line = lines.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        if let Ok(msg) = serde_json::from_str::<serde_json::Value>(l.trim()) {
                            debug!("keepalive: method={:?}", msg.get("method"));
                        }
                    }
                    Ok(None) => { info!("矿池关闭"); break; }
                    Err(e) => return Err(Error::Network(e.to_string())),
                }
            }
        }
    }
    Ok(())
}

async fn handle_msg(
    msg: &serde_json::Value,
    job_tx: &mut watch::Sender<Option<Job>>,
    event_tx: &mut mpsc::Sender<StratumEvent>,
    _keepalive: bool,
) -> Result<()> {
    // "job" 通知（c3pool 的 mining.notify）
    if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
        match method {
            "job" => {
                if let Some(params) = msg.get("params") {
                    if let Some(job) = Job::from_c3pool_job(params) {
                        debug!("job: id={} height={:?} algo={}", job.job_id, job.height, job.algo);
                        let _ = job_tx.send(Some(job.clone())).ok();
                        let _ = event_tx.send(StratumEvent::NewJob(job)).await;
                    }
                }
            }
            _ => debug!("method: {}", method),
        }
        return Ok(());
    }

    // 响应消息
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

// ══════════════════════════════════════════

fn build_login(user: &str, pass: &str, id: u64) -> String {
    // c3pool login 格式：参数是对象，包含 login / pass / agent / algo 数组
    let mut s = format!(
        "{{\"id\":{},\"jsonrpc\":\"2.0\",\"method\":\"login\",\"params\":{{\"login\":\"{}\",\"pass\":\"{}\",\"agent\":\"{}\",\"algo\":[",
        id, user, pass, APP_USER_AGENT
    );
    for (i, algo) in SUPPORTED_ALGOS.iter().enumerate() {
        if i > 0 { s.push(','); }
        s.push('"');
        s.push_str(algo);
        s.push('"');
    }
    s.push_str("]}}\n");
    s
}

fn build_submit(_user: &str, session_id: &str, job_id: &str, nonce: &str, result: &str, id: u64) -> String {
    // c3pool submit 格式：参数是对象
    format!(
        "{{\"id\":{},\"jsonrpc\":\"2.0\",\"method\":\"submit\",\"params\":{{\"id\":\"{}\",\"job_id\":\"{}\",\"nonce\":\"{}\",\"result\":\"{}\"}}}}\n",
        id, session_id, job_id, nonce, result
    )
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