use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::job::Job;

use super::transport::StratumTransport;

pub const APP_USER_AGENT: &str =
    "XMRig/6.26.0 (Windows NT 10.0; Win64; x64) libuv/1.51.0 msvc/2022";

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
            info!("connecting to pool {} ...", self.url);
            match self
                .run_session(
                    &mut job_tx,
                    &mut submit_rx,
                    &mut event_tx,
                    &mut shutdown_rx,
                    keepalive,
                )
                .await
            {
                Ok(()) => {
                    info!("pool connection closed");
                    break;
                }
                Err(e) => {
                    warn!("pool connection error: {}, reconnecting in 5s...", e);
                    let _ = event_tx
                        .send(StratumEvent::Disconnected(e.to_string()))
                        .await;
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

        let login_id = self.next_id();
        let login_msg = build_login(&self.user, &self.pass, login_id);
        writer.write_all(login_msg.as_bytes()).await?;
        writer.flush().await?;
        debug!("login: id={}", login_id);

        loop {
            let line = lines
                .next_line()
                .await?
                .ok_or(Error::Stratum("pool closed during login".into()))?;
            let msg: serde_json::Value = serde_json::from_str(&line)?;
            let mid = msg.get("id").and_then(|v| v.as_u64());

            if mid == Some(login_id) {
                if let Some(e) = get_json_error(&msg) {
                    return Err(Error::Stratum(format!("login failed: {}", e)));
                }

                let mut session_id = String::new();
                if let Some(result) = msg.get("result").and_then(|v| v.as_object()) {
                    if let Some(sid) = result.get("id").and_then(|v| v.as_str()) {
                        session_id = sid.to_string();
                    }
                    if let Some(job_obj) = result.get("job") {
                        if let Some(job) = Job::from_c3pool_job(job_obj) {
                            if !is_supported_algo(&job.algo) {
                                warn!(
                                    "unsupported pool job algo '{}'; lite-xmr currently supports rx/0 only",
                                    job.algo
                                );
                            } else {
                                debug!(
                                    "login job: id={} height={:?} algo={}",
                                    job.job_id, job.height, job.algo
                                );
                                let _ = job_tx.send(Some(job.clone())).ok();
                                let _ = event_tx.send(StratumEvent::NewJob(job)).await;
                            }
                        }
                    }
                }

                info!("pool login ok session={}", session_id);
                let _ = event_tx.send(StratumEvent::Connected).await;

                if keepalive {
                    keepalive_loop(&mut lines, shutdown_rx).await?;
                } else {
                    mining_loop(
                        &mut lines,
                        shutdown_rx,
                        submit_rx,
                        &mut writer,
                        self,
                        job_tx,
                        event_tx,
                        &session_id,
                    )
                    .await?;
                }
                return Ok(());
            }

            handle_msg(&msg, job_tx, event_tx).await?;
        }
    }

    #[allow(dead_code)]
    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
    }

    fn next_id(&self) -> u64 {
        self.request_id.fetch_add(1, Ordering::Relaxed)
    }
}

async fn mining_loop(
    lines: &mut tokio::io::Lines<BufReader<tokio::io::ReadHalf<StratumTransport>>>,
    shutdown_rx: &mut watch::Receiver<bool>,
    submit_rx: &mut mpsc::Receiver<(String, String, String)>,
    writer: &mut tokio::io::WriteHalf<StratumTransport>,
    client: &StratumClient,
    job_tx: &mut watch::Sender<Option<Job>>,
    event_tx: &mut mpsc::Sender<StratumEvent>,
    session_id: &str,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                info!("shutdown signal received");
                break;
            }
            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        let line = line.trim().to_string();
                        if line.is_empty() {
                            continue;
                        }
                        let msg: serde_json::Value = match serde_json::from_str(&line) {
                            Ok(m) => m,
                            Err(e) => {
                                warn!("JSON error: {} (line: {})", e, &line[..line.len().min(120)]);
                                continue;
                            }
                        };
                        handle_msg(&msg, job_tx, event_tx).await?;
                    }
                    Ok(None) => {
                        info!("pool closed connection");
                        break;
                    }
                    Err(e) => return Err(Error::Network(e.to_string())),
                }
            }
            Some((job_id, nonce, result)) = submit_rx.recv() => {
                let id = client.next_id();
                let msg = build_submit(session_id, &job_id, &nonce, &result, id);
                debug!("submit: id={} session={} job={} nonce={}", id, session_id, job_id, nonce);
                if let Err(e) = writer.write_all(msg.as_bytes()).await {
                    return Err(Error::Network(e.to_string()));
                }
                if let Err(e) = writer.flush().await {
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
                break;
            }
            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        if let Ok(msg) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                            debug!("keepalive: method={:?}", msg.get("method"));
                        }
                    }
                    Ok(None) => {
                        info!("pool closed");
                        break;
                    }
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
) -> Result<()> {
    if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
        match method {
            "job" => {
                if let Some(params) = msg.get("params") {
                    if let Some(job) = Job::from_c3pool_job(params) {
                        if !is_supported_algo(&job.algo) {
                            warn!(
                                "unsupported pool job algo '{}'; lite-xmr currently supports rx/0 only",
                                job.algo
                            );
                            return Ok(());
                        }
                        debug!(
                            "job: id={} height={:?} algo={}",
                            job.job_id, job.height, job.algo
                        );
                        let _ = job_tx.send(Some(job.clone())).ok();
                        let _ = event_tx.send(StratumEvent::NewJob(job)).await;
                    }
                }
            }
            _ => debug!("method: {}", method),
        }
        return Ok(());
    }

    if msg.get("id").is_some() {
        match msg.get("error") {
            None | Some(serde_json::Value::Null) => {
                let _ = event_tx.send(StratumEvent::Accepted).await;
            }
            Some(err) => {
                let _ = event_tx
                    .send(StratumEvent::Rejected(format_json_error(err)))
                    .await;
            }
        }
    }

    Ok(())
}

fn build_login(user: &str, pass: &str, id: u64) -> String {
    let mut s = format!(
        "{{\"id\":{},\"jsonrpc\":\"2.0\",\"method\":\"login\",\"params\":{{\"login\":\"{}\",\"pass\":\"{}\",\"agent\":\"{}\",\"algo\":[",
        id, user, pass, APP_USER_AGENT
    );
    for (i, algo) in SUPPORTED_ALGOS.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push('"');
        s.push_str(algo);
        s.push('"');
    }
    s.push_str("]}}\n");
    s
}

fn build_submit(session_id: &str, job_id: &str, nonce: &str, result: &str, id: u64) -> String {
    format!(
        "{{\"id\":{},\"jsonrpc\":\"2.0\",\"method\":\"submit\",\"params\":{{\"id\":\"{}\",\"job_id\":\"{}\",\"nonce\":\"{}\",\"result\":\"{}\"}}}}\n",
        id, session_id, job_id, nonce, result
    )
}

fn is_supported_algo(algo: &str) -> bool {
    matches!(algo, "rx/0" | "randomx" | "randomx/0")
}

fn get_json_error(msg: &serde_json::Value) -> Option<String> {
    let err = msg.get("error")?;
    if err.is_null() {
        return None;
    }
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
    if let Some(s) = err.get("message").and_then(|m| m.as_str()) {
        return Some(s.to_string());
    }
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
    err.get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_string()
}
