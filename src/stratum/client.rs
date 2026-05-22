use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::job::Job;

use super::transport::StratumTransport;

pub const APP_USER_AGENT: &str = "lite-xmr/0.1.0";

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

        // --- 阶段 1: 发送 subscribe，读取响应（可能夹杂 mining.notify） ---
        let subscribe_id = self.next_id();
        let subscribe_msg = format!(
            "{{\"id\":{},\"method\":\"mining.subscribe\",\"params\":[\"{}\",\"rx/0\"]}}\n",
            subscribe_id, APP_USER_AGENT
        );
        writer.write_all(subscribe_msg.as_bytes()).await?;
        debug!("已发送订阅请求 id={}", subscribe_id);

        // 循环读取，直到收到 subscribe 响应
        loop {
            let line = lines
                .next_line()
                .await?
                .ok_or(Error::Stratum("矿池在订阅阶段关闭了连接".into()))?;

            let msg: serde_json::Value = serde_json::from_str(&line)?;

            // 检查是否是我们的 subscribe 响应
            if msg.get("id").and_then(|v| v.as_u64()) == Some(subscribe_id) {
                debug!("订阅响应: {}", line.trim());
                if let Some(err) = msg.get("error").and_then(|e| e.as_object()) {
                    if !err.is_empty() {
                        let reason = err
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("未知原因");
                        return Err(Error::Stratum(format!("订阅失败: {}", reason)));
                    }
                }
                break;
            }

            // 可能是矿池在订阅响应前就推送了 mining.notify 或其它消息
            self.handle_incoming(&msg, job_tx, event_tx, keepalive).await?;
        }

        // --- 阶段 2: 发送 authorize，读取响应（可能夹杂 mining.notify） ---
        let auth_id = self.next_id();
        let auth_msg = format!(
            "{{\"id\":{},\"method\":\"mining.authorize\",\"params\":[\"{}\",\"{}\"]}}\n",
            auth_id, self.user, self.pass
        );
        writer.write_all(auth_msg.as_bytes()).await?;
        debug!("已发送授权请求 id={}", auth_id);

        loop {
            let line = lines
                .next_line()
                .await?
                .ok_or(Error::Stratum("矿池在授权阶段关闭了连接".into()))?;

            let msg: serde_json::Value = serde_json::from_str(&line)?;

            // 检查是否是我们的 auth 响应
            if msg.get("id").and_then(|v| v.as_u64()) == Some(auth_id) {
                debug!("授权响应: {}", line.trim());
                if msg.get("result").and_then(|r| r.as_bool()).unwrap_or(false) {
                    info!("矿池授权成功");
                    let _ = event_tx.send(StratumEvent::Connected).await;
                    break;
                } else {
                    let reason = msg
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("未知原因");
                    return Err(Error::Stratum(format!("授权失败: {}", reason)));
                }
            }

            // 可能是矿池在授权响应前推送的消息（mining.notify / set_extranonce 等）
            self.handle_incoming(&msg, job_tx, event_tx, keepalive).await?;
        }

        // --- 阶段 3: 主事件循环 ---
        if keepalive {
            info!("保活模式: 保持连接不挖矿");
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
                                if line.is_empty() {
                                    continue;
                                }
                                let msg: serde_json::Value = match serde_json::from_str(&line) {
                                    Ok(m) => m,
                                    Err(_) => continue,
                                };
                                if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                                    match method {
                                        "mining.notify" => {
                                            debug!("保活模式: 忽略 mining.notify");
                                        }
                                        "mining.set_target" => {
                                            debug!("保活模式: 忽略 mining.set_target");
                                        }
                                        "mining.set_extranonce" => {
                                            debug!("保活模式: 忽略 mining.set_extranonce");
                                        }
                                        _ => {
                                            debug!("保活模式: 忽略方法 {}", method);
                                        }
                                    }
                                }
                                if msg.get("id").is_some() && msg.get("result").is_some() {
                                    debug!("保活模式: 收到响应 id={:?}", msg.get("id"));
                                }
                            }
                            Ok(None) => {
                                info!("矿池关闭了连接");
                                break;
                            }
                            Err(e) => {
                                return Err(Error::Network(e.to_string()));
                            }
                        }
                    }
                }
            }
        } else {
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
                                if line.is_empty() {
                                    continue;
                                }
                                let msg: serde_json::Value = match serde_json::from_str(&line) {
                                    Ok(m) => m,
                                    Err(e) => {
                                        warn!("JSON 解析错误: {} (line: {})", e, &line[..line.len().min(120)]);
                                        continue;
                                    }
                                };
                                self.handle_incoming(&msg, job_tx, event_tx, false).await?;
                            }
                            Ok(None) => {
                                info!("矿池关闭了连接");
                                break;
                            }
                            Err(e) => {
                                return Err(Error::Network(e.to_string()));
                            }
                        }
                    }

                    Some((job_id, nonce, result)) = submit_rx.recv() => {
                        let id = self.next_id();
                        let submit_msg = format!(
                            "{{\"id\":{},\"method\":\"mining.submit\",\"params\":[\"{}\",\"{}\",\"{}\",\"{}\"]}}\n",
                            id, self.user, job_id, nonce, result
                        );
                        if let Err(e) = writer.write_all(submit_msg.as_bytes()).await {
                            return Err(Error::Network(e.to_string()));
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// 统一的消息分发：处理 mining.notify / set_extranonce / set_target / submit 响应
    async fn handle_incoming(
        &self,
        msg: &serde_json::Value,
        job_tx: &mut watch::Sender<Option<Job>>,
        event_tx: &mut mpsc::Sender<StratumEvent>,
        keepalive: bool,
    ) -> Result<()> {
        // 1) 带 method 字段的推送消息
        if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
            match method {
                "mining.notify" => {
                    if keepalive {
                        debug!("忽略 mining.notify (保活模式)");
                        return Ok(());
                    }
                    if let Some(params) = msg.get("params").and_then(|p| p.as_array()) {
                        if let Some(job) = Job::from_notify(params) {
                            debug!("新任务: job_id={}, height={:?}, algo={}", job.job_id, job.height, job.algo);
                            let _ = job_tx.send(Some(job.clone())).ok();
                            let _ = event_tx.send(StratumEvent::NewJob(job)).await;
                        }
                    }
                }
                "mining.set_target" => {
                    debug!("收到 mining.set_target (目标难度更新)");
                    // 大多数矿池通过 mining.notify 的 target 字段控制难度，这里暂不处理
                }
                "mining.set_extranonce" => {
                    debug!("收到 mining.set_extranonce (已记录，使用默认 nonce 偏移)");
                    // Monero 标准: extranonce 内嵌在 blob 中，nonce 偏移固定为 39
                }
                _ => {
                    debug!("未知方法: {}", method);
                }
            }
            return Ok(());
        }

        // 2) submit 响应（带 id + result/error）
        if msg.get("id").is_some() {
            match msg.get("error") {
                None | Some(serde_json::Value::Null) => {
                    let _ = event_tx.send(StratumEvent::Accepted).await;
                }
                Some(err) => {
                    let reason = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("未知错误");
                    let _ = event_tx.send(StratumEvent::Rejected(reason.to_string())).await;
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
