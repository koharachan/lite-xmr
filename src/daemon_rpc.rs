use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;
use serde_json::json;

use crate::job::Job;

#[derive(Clone)]
pub struct DaemonRpcClient {
    rpc_url: String,
    wallet: String,
    rpc_login: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RpcEnvelope<T> {
    result: Option<T>,
    error: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct BlockTemplateResult {
    blocktemplate_blob: String,
    blockhashing_blob: String,
    difficulty: u64,
    height: u64,
    seed_hash: Option<String>,
    next_seed_hash: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SubmitBlockResult {
    status: Option<String>,
}

impl DaemonRpcClient {
    pub fn new(url: &str, wallet: &str, rpc_login: Option<String>) -> Self {
        DaemonRpcClient {
            rpc_url: normalize_rpc_url(url),
            wallet: wallet.to_string(),
            rpc_login: rpc_login.filter(|s| !s.trim().is_empty()),
        }
    }

    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    pub fn get_block_template(&self) -> anyhow::Result<Job> {
        let result: BlockTemplateResult = self.json_rpc(
            "get_block_template",
            json!({
                "wallet_address": self.wallet,
                "reserve_size": 0,
            }),
        )?;
        let seed_hash = result.seed_hash.or(result.next_seed_hash);
        let job_id = daemon_job_id(result.height, &result.blockhashing_blob);

        Job::from_daemon_template(
            job_id,
            result.blockhashing_blob,
            result.blocktemplate_blob,
            result.difficulty,
            Some(result.height),
            seed_hash,
        )
        .context("daemon returned an invalid block template")
    }

    pub fn submit_block(&self, block_blob: &str) -> anyhow::Result<()> {
        let result: SubmitBlockResult = self.json_rpc("submit_block", json!([block_blob]))?;
        let status = result.status.unwrap_or_else(|| "OK".to_string());
        if status.eq_ignore_ascii_case("OK") {
            Ok(())
        } else {
            anyhow::bail!("daemon submit_block status={}", status)
        }
    }

    fn json_rpc<T>(&self, method: &str, params: serde_json::Value) -> anyhow::Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let body = json!({
            "jsonrpc": "2.0",
            "id": "0",
            "method": method,
            "params": params,
        });
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(15)))
                .build(),
        );
        let mut request = agent.post(&self.rpc_url).content_type("application/json");
        if let Some(login) = self.rpc_login.as_deref() {
            request = request.header("Authorization", format!("Basic {}", base64(login)));
        }
        let mut response = request
            .send(serde_json::to_string(&body)?)
            .with_context(|| format!("daemon RPC {} failed", method))?;
        let bytes = response
            .body_mut()
            .read_to_vec()
            .with_context(|| format!("daemon RPC {} response read failed", method))?;
        let envelope: RpcEnvelope<T> = serde_json::from_slice(&bytes)
            .with_context(|| format!("daemon RPC {} returned invalid JSON", method))?;

        if let Some(error) = envelope.error {
            anyhow::bail!("daemon RPC {} error: {}", method, error);
        }
        envelope
            .result
            .with_context(|| format!("daemon RPC {} returned no result", method))
    }
}

fn normalize_rpc_url(raw: &str) -> String {
    let mut url = raw.trim().to_string();
    if let Some(rest) = url.strip_prefix("daemon+") {
        url = rest.to_string();
    }
    if !url.contains("://") {
        url = format!("http://{}", url);
    }
    if !url.ends_with("/json_rpc") {
        url = url.trim_end_matches('/').to_string();
        url.push_str("/json_rpc");
    }
    url
}

fn daemon_job_id(height: u64, blob: &str) -> String {
    let prefix = blob.get(..16).unwrap_or(blob);
    format!("daemon-{}-{}", height, prefix)
}

fn base64(input: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);

    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_daemon_rpc_urls() {
        assert_eq!(
            normalize_rpc_url("daemon+http://127.0.0.1:18081"),
            "http://127.0.0.1:18081/json_rpc"
        );
        assert_eq!(
            normalize_rpc_url("127.0.0.1:18081"),
            "http://127.0.0.1:18081/json_rpc"
        );
        assert_eq!(
            normalize_rpc_url("http://node.example/json_rpc"),
            "http://node.example/json_rpc"
        );
    }

    #[test]
    fn encodes_basic_auth() {
        assert_eq!(base64("user:pass"), "dXNlcjpwYXNz");
    }
}
