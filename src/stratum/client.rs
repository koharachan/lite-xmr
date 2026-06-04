use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::algorithms;
use crate::error::{Error, Result};
use crate::job::Job;

use super::transport::StratumTransport;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginProfile {
    Lite,
    XmrigCompat,
    Minimal,
}

impl LoginProfile {
    fn name(self) -> &'static str {
        match self {
            LoginProfile::Lite => "lite",
            LoginProfile::XmrigCompat => "xmrig-compat",
            LoginProfile::Minimal => "minimal",
        }
    }
}

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
    sni: Option<String>,
    tls_allow_12: bool,
    tls_fingerprint: Option<String>,
    socks5: Option<String>,
    user_agent: String,
    miner_signature: Option<String>,
    http2: bool,
    http3: bool,
    ws: bool,
    algo_perf: BTreeMap<String, f64>,
    algo_min_time: Option<u64>,
    running: Arc<AtomicBool>,
    request_id: AtomicU64,
}

impl StratumClient {
    pub fn new(
        url: String,
        user: String,
        pass: String,
        use_tls: bool,
        sni: Option<String>,
        tls_allow_12: bool,
        tls_fingerprint: Option<String>,
        socks5: Option<String>,
        user_agent: String,
        miner_signature: Option<String>,
        http2: bool,
        http3: bool,
        ws: bool,
        algo_perf: BTreeMap<String, f64>,
        algo_min_time: Option<u64>,
    ) -> Self {
        StratumClient {
            url,
            user,
            pass,
            use_tls,
            sni,
            tls_allow_12,
            tls_fingerprint,
            socks5,
            user_agent,
            miner_signature,
            http2,
            http3,
            ws,
            algo_perf: algorithms::filtered_algo_perf(&algo_perf),
            algo_min_time,
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
        let profiles = [
            LoginProfile::Lite,
            LoginProfile::XmrigCompat,
            LoginProfile::Minimal,
        ];
        let mut last_error = None;

        for (idx, profile) in profiles.iter().copied().enumerate() {
            match self
                .run_session_with_profile(
                    job_tx,
                    submit_rx,
                    event_tx,
                    shutdown_rx,
                    keepalive,
                    profile,
                )
                .await
            {
                Ok(()) => return Ok(()),
                Err(e) if idx + 1 < profiles.len() && is_login_retryable(&e) => {
                    warn!(
                        "pool login with {} profile failed: {}; retrying with {} profile",
                        profile.name(),
                        e,
                        profiles[idx + 1].name()
                    );
                    last_error = Some(e);
                }
                Err(e) => return Err(e),
            }
        }

        Err(last_error.unwrap_or_else(|| Error::Stratum("pool login failed".into())))
    }

    async fn run_session_with_profile(
        &self,
        job_tx: &mut watch::Sender<Option<Job>>,
        submit_rx: &mut mpsc::Receiver<(String, String, String)>,
        event_tx: &mut mpsc::Sender<StratumEvent>,
        shutdown_rx: &mut watch::Receiver<bool>,
        keepalive: bool,
        profile: LoginProfile,
    ) -> Result<()> {
        let transport = StratumTransport::connect(
            &self.url,
            self.use_tls,
            self.sni.as_deref(),
            self.tls_allow_12,
            self.tls_fingerprint.as_deref(),
            self.socks5.as_deref(),
        )
        .await
        .map_err(|e| Error::Network(e.to_string()))?;
        let (reader, mut writer) = tokio::io::split(transport);
        let mut lines = BufReader::new(reader).lines();

        let login_id = self.next_id();
        let login_msg = build_login(
            &self.user,
            &self.pass,
            &self.user_agent,
            self.miner_signature.as_deref(),
            self.http2,
            self.http3,
            self.ws,
            &self.algo_perf,
            self.algo_min_time,
            login_id,
            profile,
        );
        writer.write_all(login_msg.as_bytes()).await?;
        writer.flush().await?;
        debug!(
            "login: id={} profile={} agent={:?} http2={} http3={} ws={}",
            login_id,
            profile.name(),
            self.user_agent,
            self.http2,
            self.http3,
            self.ws
        );

        loop {
            let line = lines
                .next_line()
                .await?
                .ok_or(Error::Stratum("pool closed during login".into()))?;
            let msg: serde_json::Value = parse_json_line(&line)?;
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
                                    "unsupported pool job algo '{}'; not declared by this build",
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

                info!(
                    "pool login ok session={} profile={}",
                    session_id,
                    profile.name()
                );
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
                        let msg: serde_json::Value = match parse_json_line(&line) {
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
                    Err(e) => return Err(network_read_error(e)),
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
                        if let Ok(msg) = parse_json_line(line.trim()) {
                            debug!("keepalive: method={:?}", msg.get("method"));
                        }
                    }
                    Ok(None) => {
                        info!("pool closed");
                        break;
                    }
                    Err(e) => return Err(network_read_error(e)),
                }
            }
        }
    }
    Ok(())
}

fn network_read_error(e: std::io::Error) -> Error {
    let msg = e.to_string();
    if is_noisy_tls_close(&msg) {
        Error::Network("TLS stream closed by pool".into())
    } else {
        Error::Network(msg)
    }
}

fn is_noisy_tls_close(msg: &str) -> bool {
    msg.contains("decryption failed or bad record mac") || msg.contains("record layer failure")
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
                                "unsupported pool job algo '{}'; not declared by this build",
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
            "mining.notify" => {
                if let Some(params) = msg.get("params").and_then(|v| v.as_array()) {
                    if let Some(job) = Job::from_notify(params) {
                        if !is_supported_algo(&job.algo) {
                            warn!(
                                "unsupported pool job algo '{}'; not declared by this build",
                                job.algo
                            );
                            return Ok(());
                        }
                        debug!(
                            "mining.notify: id={} height={:?} algo={}",
                            job.job_id, job.height, job.algo
                        );
                        let _ = job_tx.send(Some(job.clone())).ok();
                        let _ = event_tx.send(StratumEvent::NewJob(job)).await;
                    }
                }
            }
            "mining.set_difficulty" => {
                debug!("method: {}", method);
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

fn build_login(
    user: &str,
    pass: &str,
    user_agent: &str,
    miner_signature: Option<&str>,
    http2: bool,
    http3: bool,
    ws: bool,
    algo_perf: &BTreeMap<String, f64>,
    algo_min_time: Option<u64>,
    id: u64,
    profile: LoginProfile,
) -> String {
    let algos = match profile {
        LoginProfile::Lite | LoginProfile::XmrigCompat => algorithms::SUPPORTED_ALGOS,
        LoginProfile::Minimal => &[][..],
    };

    let mut params = serde_json::json!({
        "login": user,
        "pass": pass,
        "agent": user_agent,
        "coin": "monero",
    });

    if !algos.is_empty() {
        params["algo"] = serde_json::json!(algos);
    }
    if !algo_perf.is_empty() {
        params["algo-perf"] = serde_json::json!(algo_perf);
    }
    if let Some(seconds) = algo_min_time.filter(|v| *v > 0) {
        params["algo-min-time"] = serde_json::json!(seconds);
    }
    if let Some(sig) = miner_signature.map(str::trim).filter(|s| !s.is_empty()) {
        params["sig"] = serde_json::Value::String(sig.to_string());
    }
    if http2 {
        params["http2"] = serde_json::Value::Bool(true);
    }
    if http3 {
        params["http3"] = serde_json::Value::Bool(true);
    }
    if ws {
        params["ws"] = serde_json::Value::Bool(true);
    }

    let msg = serde_json::json!({
        "id": id,
        "jsonrpc": "2.0",
        "method": "login",
        "params": params,
    });

    format!("{}\n", msg)
}

fn is_login_retryable(error: &Error) -> bool {
    match error {
        Error::Stratum(msg) => {
            msg.contains("pool closed during login") || msg.contains("login failed")
        }
        Error::Network(msg) => {
            msg.contains("unexpected EOF")
                || msg.contains("TLS stream closed")
                || msg.contains("handshake failure")
        }
        Error::Io(e) => {
            matches!(
                e.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
            )
        }
        _ => false,
    }
}

fn build_submit(session_id: &str, job_id: &str, nonce: &str, result: &str, id: u64) -> String {
    format!(
        "{{\"id\":{},\"jsonrpc\":\"2.0\",\"method\":\"submit\",\"params\":{{\"id\":\"{}\",\"job_id\":\"{}\",\"nonce\":\"{}\",\"result\":\"{}\"}}}}\n",
        id, session_id, job_id, nonce, result
    )
}

fn is_supported_algo(algo: &str) -> bool {
    algorithms::is_supported(algo)
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

fn parse_json_line(line: &str) -> std::result::Result<serde_json::Value, serde_json::Error> {
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(v) => Ok(v),
        Err(primary) => {
            if let Some(minified) = crate::native_bridge::rapidjson_minify(line) {
                match serde_json::from_str::<serde_json::Value>(&minified) {
                    Ok(v) => return Ok(v),
                    Err(_) => {}
                }
            }
            Err(primary)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_can_advertise_http3_and_ws_capabilities() {
        let algo_perf = BTreeMap::from([(algorithms::RX0.to_string(), 488.0)]);
        let msg = build_login(
            "wallet",
            "x",
            "agent",
            Some("signature"),
            true,
            true,
            true,
            &algo_perf,
            Some(60),
            7,
            LoginProfile::Lite,
        );
        let value: serde_json::Value = serde_json::from_str(&msg).unwrap();
        let params = value.get("params").unwrap();

        assert_eq!(params.get("coin").and_then(|v| v.as_str()), Some("monero"));
        assert_eq!(params.get("http2").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(params.get("http3").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(params.get("ws").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            params.get("sig").and_then(|v| v.as_str()),
            Some("signature")
        );
        assert_eq!(
            params
                .get("algo-perf")
                .and_then(|v| v.get(algorithms::RX0))
                .and_then(|v| v.as_f64()),
            Some(488.0)
        );
        assert_eq!(
            params.get("algo-min-time").and_then(|v| v.as_u64()),
            Some(60)
        );
    }

    #[test]
    fn xmrig_compat_login_only_advertises_real_supported_algos() {
        let algo_perf = BTreeMap::from([(algorithms::RX0.to_string(), 488.0)]);
        let msg = build_login(
            "wallet",
            "x",
            "agent",
            None,
            false,
            false,
            false,
            &algo_perf,
            None,
            8,
            LoginProfile::XmrigCompat,
        );
        let value: serde_json::Value = serde_json::from_str(&msg).unwrap();
        let algos = value
            .get("params")
            .and_then(|p| p.get("algo"))
            .and_then(|v| v.as_array())
            .unwrap();

        assert!(algos.iter().any(|v| v.as_str() == Some("rx/0")));
        assert_eq!(algos.len(), algorithms::SUPPORTED_ALGOS.len());
        assert!(
            algos
                .iter()
                .all(|v| { v.as_str().map(algorithms::is_supported).unwrap_or(false) })
        );
    }

    #[test]
    fn minimal_login_omits_algo_for_strict_legacy_pools() {
        let algo_perf = BTreeMap::from([(algorithms::RX0.to_string(), 488.0)]);
        let msg = build_login(
            "wallet",
            "x",
            "agent",
            None,
            false,
            false,
            false,
            &algo_perf,
            None,
            9,
            LoginProfile::Minimal,
        );
        let value: serde_json::Value = serde_json::from_str(&msg).unwrap();
        let params = value.get("params").unwrap();

        assert!(params.get("algo").is_none());
        assert_eq!(params.get("coin").and_then(|v| v.as_str()), Some("monero"));
    }

    #[test]
    fn supports_monero_randomx_aliases() {
        for algo in ["rx/0", "randomx", "randomx/0"] {
            assert!(is_supported_algo(algo));
        }
        assert!(!is_supported_algo("rx/wow"));
    }

    #[test]
    fn does_not_declare_unsupported_cpu_auto_algos() {
        let configured = BTreeMap::from([
            (algorithms::RX0.to_string(), 488.0),
            ("cn/r".to_string(), 100.0),
        ]);
        let filtered = algorithms::filtered_algo_perf(&configured);

        assert!(filtered.contains_key(algorithms::RX0));
        assert!(!filtered.contains_key("cn/r"));
    }
}
