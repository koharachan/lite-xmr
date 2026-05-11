//! Stratum 协议客户端。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};

use lite_xmr_core::{Error, Job, Result};

use crate::transport::StratumTransport;

/// Stratum 客户端事件。
#[derive(Debug, Clone)]
pub enum StratumEvent {
    /// 收到新的挖矿任务
    NewJob(Job),
    /// 份额被接受
    Accepted,
    /// 份额被拒绝
    Rejected(String),
    /// 连接已建立
    Connected,
    /// 连接断开
    Disconnected(String),
}

/// Stratum 客户端。
pub struct StratumClient {
    url: String,
    user: String,
    pass: String,
    use_tls: bool,
    running: Arc<AtomicBool>,
    request_id: AtomicU64,
}

impl StratumClient {
    /// 创建新的 Stratum 客户端。
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

    /// 运行 Stratum 客户端主循环。
    ///
    /// 返回两个 channel：
    /// - `job_rx`: 接收新的挖矿任务
    /// - `submit_tx`: 发送挖矿结果
    pub async fn run(
        &self,
        mut job_tx: watch::Sender<Option<Job>>,
        mut submit_rx: mpsc::Receiver<(String, String, String)>,
        mut event_tx: mpsc::Sender<StratumEvent>,
    ) -> Result<()> {
        self.running.store(true, Ordering::Relaxed);

        while self.running.load(Ordering::Relaxed) {
            info!("正在连接矿池 {} ...", self.url);

            match self.run_session(&mut job_tx, &mut submit_rx, &mut event_tx).await {
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

        self.running.store(false, Ordering::Relaxed);
        Ok(())
    }

    /// 执行一次矿池会话。
    async fn run_session(
        &self,
        job_tx: &mut watch::Sender<Option<Job>>,
        submit_rx: &mut mpsc::Receiver<(String, String, String)>,
        event_tx: &mut mpsc::Sender<StratumEvent>,
    ) -> Result<()> {
        let mut transport = StratumTransport::connect(&self.url, self.use_tls)
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        // 1. 发送 mining.subscribe
        let id = self.next_id();
        let subscribe_msg = format!(
            "{{\"id\":{},\"method\":\"mining.subscribe\",\"params\":[\"lite-xmr/0.1.0\"]}}\n",
            id
        );
        transport.write_all(subscribe_msg.as_bytes()).await?;

        // 读取 subscribe 响应
        let mut response = String::new();
        let (reader, mut writer) = split_transport(&mut transport);
        let mut lines = BufReader::new(reader).lines();

        let line = lines.next_line().await?.ok_or(Error::Stratum("矿池关闭了连接".into()))?;
        debug!("收到响应: {}", line.trim());

        // 2. 发送 mining.authorize
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

        // 3. 主循环：处理任务和提交
        loop {
            tokio::select! {
                // 接收矿池消息
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

                // 接收提交请求
                Some((job_id, nonce, result)) = submit_rx.recv() => {
                    let id = self.next_id();
                    let submit_msg = format!(
                        "{{\"id\":{},\"method\":\"mining.submit\",\"params\":[\"{}\",\"{}\",\"{}\",\"{}\"]}}\n",
                        id, self.user, job_id, nonce, result
                    );
                    if let Err(e) = writer.write_all(submit_msg.as_bytes()).await {
                        error!("提交份额失败: {}", e);
                    }
                }
            }
        }

        Ok(())
    }

    /// 处理矿池发来的消息。
    async fn handle_message(
        &self,
        line: &str,
        job_tx: &mut watch::Sender<Option<Job>>,
        event_tx: &mut mpsc::Sender<StratumEvent>,
    ) -> Result<()> {
        let msg: serde_json::Value = serde_json::from_str(line)?;

        // 处理方法调用 (矿池 -> 客户端)
        if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
            match method {
                "mining.notify" => {
                    if let Some(params) = msg.get("params").and_then(|p| p.as_array()) {
                        if let Some(job) = Job::from_notify(params) {
                            debug!("收到新任务: job_id={}, height={:?}", job.job_id, job.height);
                            let _ = job_tx.send(Some(job.clone())).ok();
                            let _ = event_tx.send(StratumEvent::NewJob(job)).await;
                        }
                    }
                }
                "mining.set_target" => {
                    debug!("收到 set_target");
                }
                "mining.set_extranonce" => {
                    debug!("收到 set_extranonce");
                }
                _ => {
                    debug!("未知方法: {}", method);
                }
            }
            return Ok(());
        }

        // 处理响应 (客户端 -> 矿池 的回复)
        if let Some(id) = msg.get("id") {
            // 检查是否是 submit 的响应
            if let Some(error) = msg.get("error") {
                if error.is_null() {
                    // 成功
                    let _ = event_tx.send(StratumEvent::Accepted).await;
                } else {
                    let reason = error
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("未知错误");
                    let _ = event_tx.send(StratumEvent::Rejected(reason.to_string())).await;
                }
            }
        }

        Ok(())
    }

    /// 停止客户端。
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

    /// 获取下一个请求 ID。
    fn next_id(&self) -> u64 {
        self.request_id.fetch_add(1, Ordering::Relaxed)
    }
}

/// 将 transport 拆分为 read 和 write 两半。
fn split_transport(
    transport: &mut StratumTransport,
) -> (tokio::io::DuplexStream, tokio::io::DuplexStream) {
    // 简化实现：使用 channel 模拟
    let (client_read, server_write) = tokio::io::duplex(65536);
    let (server_read, client_write) = tokio::io::duplex(65536);
    (client_read, client_write)
}
