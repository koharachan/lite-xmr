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

        let id = self.next_id();
        let subscribe_msg = format!(
            "{{\"id\":{},\"method\":\"mining.subscribe\",\"params\":[\"{}\"]}}\n",
            id, APP_USER_AGENT
        );
        writer.write_all(subscribe_msg.as_bytes()).await?;

        let line = lines.next_line().await?.ok_or(Error::Stratum("矿池关闭了连接".into()))?;
        debug!("订阅响应: {}", line.trim());

        let id = self.next_id();
        let auth_msg = format!(
            "{{\"id\":{},\"method\":\"mining.authorize\",\"params\":[\"{}\",\"{}\"]}}\n",
            id, self.user, self.pass
        );
        writer.write_all(auth_msg.as_bytes()).await?;

        let line = lines.next_line().await?.ok_or(Error::Stratum("矿池关闭了连接".into()))?;
        debug!("授权响应: {}", line.trim());

        let auth_result: serde_json::Value = serde_json::from_str(&line)?;
        if auth_result.get("result").and_then(|r| r.as_bool()).unwrap_or(false) {
            info!("矿池授权成功");
            let _ = event_tx.send(StratumEvent::Connected).await;
        } else {
            let reason = auth_result
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("未知原因");
            return Err(Error::Stratum(format!("授权失败: {}", reason)));
        }

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
                                self.handle_message(&line, job_tx, event_tx).await?;
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

    async fn handle_message(
        &self,
        line: &str,
        job_tx: &mut watch::Sender<Option<Job>>,
        event_tx: &mut mpsc::Sender<StratumEvent>,
    ) -> Result<()> {
        let msg: serde_json::Value = serde_json::from_str(line)?;

        if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
            match method {
                "mining.notify" => {
                    if let Some(params) = msg.get("params").and_then(|p| p.as_array()) {
                        if let Some(job) = Job::from_notify(params) {
                            debug!("新任务: job_id={}, height={:?}", job.job_id, job.height);
                            let _ = job_tx.send(Some(job.clone())).ok();
                            let _ = event_tx.send(StratumEvent::NewJob(job)).await;
                        }
                    }
                }
                "mining.set_target" => {
                    warn!("mining.set_target 收到但未实现");
                }
                "mining.set_extranonce" => {
                    warn!("mining.set_extranonce 收到但未实现");
                }
                _ => {
                    debug!("未知方法: {}", method);
                }
            }
            return Ok(());
        }

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
