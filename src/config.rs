use pico_args::Arguments;
use serde::{Deserialize, Deserializer};
use std::ffi::OsString;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use crate::cpu::APP_VERSION;

#[derive(Debug, Clone)]
pub enum EarlyExit {
    Help,
    Version,
}

#[derive(Debug, Clone)]
pub struct Args {
    pub url: Option<String>,
    pub user: Option<String>,
    pub pass: Option<String>,
    pub threads: Option<u32>,
    pub tls: bool,
    pub sni: Option<String>,
    pub user_agent: Option<String>,
    pub http2: bool,
    pub http3: bool,
    pub ws: bool,
    pub config: Option<PathBuf>,
    pub log_level: Option<String>,
    pub api_bind: Option<SocketAddr>,
    pub keepalive: bool,
    pub doh: bool,
    pub background: bool,
    pub bench_seconds: Option<u64>,
    pub use_e_cores: bool,
    pub verbose: bool,
}

impl Args {
    pub fn parse() -> anyhow::Result<Result<Self, EarlyExit>> {
        let mut pargs = Arguments::from_vec(normalized_args_from_env());

        if pargs.contains("-h") || pargs.contains("--help") {
            print_usage();
            return Ok(Err(EarlyExit::Help));
        }
        if pargs.contains("-v") || pargs.contains("--version") {
            println!("lite-xmr v{}", APP_VERSION);
            return Ok(Err(EarlyExit::Version));
        }

        let url = pargs
            .opt_value_from_str(["-o", "--url"])?
            .or(pargs.opt_value_from_str("--pool")?);
        let user_agent = pargs.opt_value_from_str("--ua")?;

        let args = Args {
            url,
            user: pargs.opt_value_from_str(["-u", "--user"])?,
            pass: pargs.opt_value_from_str(["-p", "--pass"])?,
            threads: pargs.opt_value_from_str(["-t", "--threads"])?,
            tls: pargs.contains("--tls"),
            sni: pargs.opt_value_from_str("--sni")?,
            user_agent,
            http2: pargs.contains("--http2"),
            http3: pargs.contains("--http3"),
            ws: pargs.contains("--ws"),
            config: pargs.opt_value_from_str("--config")?,
            log_level: pargs.opt_value_from_str("--log-level")?,
            api_bind: pargs.opt_value_from_str("--api-bind")?,
            keepalive: pargs.contains(["-k", "--keepalive"]),
            doh: pargs.contains("--doh"),
            background: pargs.contains(["-B", "--background"]),
            bench_seconds: pargs.opt_value_from_str("--bench")?,
            use_e_cores: pargs.contains("--use-e-cores"),
            verbose: pargs.contains(["-V", "--verbose"]),
        };

        let remaining = pargs.finish();
        if !remaining.is_empty() {
            anyhow::bail!("unknown arguments: {:?}", remaining);
        }

        Ok(Ok(args))
    }
}

fn normalized_args_from_env() -> Vec<OsString> {
    std::env::args_os()
        .skip(1)
        .map(normalize_legacy_arg)
        .collect()
}

fn normalize_legacy_arg(arg: OsString) -> OsString {
    if arg == "-tls" {
        return "--tls".into();
    }
    if arg == "-ua" {
        return "--ua".into();
    }
    if let Some(arg) = arg.to_str() {
        if let Some(value) = arg.strip_prefix("-ua=") {
            return format!("--ua={}", value).into();
        }
    }
    arg
}

fn print_usage() {
    println!("lite-xmr v{} - Monero (XMR) CPU miner", APP_VERSION);
    println!();
    println!("Usage: lite-xmr [OPTIONS]");
    println!();
    println!("Options:");
    println!("  -o, --url, --pool <HOST:PORT>  Pool address");
    println!("  -u, --user <ADDRESS>            Wallet address or username");
    println!("  -p, --pass <STRING>             Pool password (default: x)");
    println!("  -t, --threads <N>               Mining threads (0 = auto)");
    println!("      --tls                       Use TLS for pool connection");
    println!("      --sni <HOST>                Override TLS SNI server name");
    println!(
        "  -ua, --ua <MODE>                User-Agent preset: default, edge, full, xmrig, fast, short, sogo, ie11"
    );
    println!("      --http2                     Advertise HTTP/2 support in login params");
    println!("      --http3                     Advertise HTTP/3 support in login params");
    println!("      --ws                        Advertise WebSocket support in login params");
    println!("      --config <PATH>             Config file (.json/.toml)");
    println!("      --log-level <LEVEL>         Log level (default: info)");
    println!("  -V, --verbose                   Shortcut for --log-level debug");
    println!("      --api-bind <ADDR>           HTTP API bind address");
    println!("  -k, --keepalive                 Keep connection alive (no mining)");
    println!("      --doh                       Resolve pool host via DoH");
    println!("  -B, --background                Run in background mode");
    println!("  -h, --help                      Show this help");
    println!("  -v, --version                   Show version");
}

#[derive(Debug, Clone, Deserialize)]
pub struct PoolConfig {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub pass: Option<String>,
    #[serde(default = "default_pool_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub tls: bool,
    #[serde(default, deserialize_with = "deserialize_pool_sni")]
    pub sni: Option<PoolSni>,
    #[serde(default, alias = "user-agent", alias = "user_agent")]
    pub ua: Option<String>,
    #[serde(default)]
    pub http2: bool,
    #[serde(default)]
    pub http3: bool,
    #[serde(default)]
    pub ws: bool,
    #[serde(default, deserialize_with = "deserialize_keepalive")]
    pub keepalive: bool,
    #[serde(default)]
    pub doh: bool,
}

fn default_pool_enabled() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolSni {
    Disabled,
    Enabled,
    Name(String),
}

fn deserialize_pool_sni<'de, D>(deserializer: D) -> std::result::Result<Option<PoolSni>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    let Some(value) = value else {
        return Ok(None);
    };

    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Bool(true) => Ok(Some(PoolSni::Enabled)),
        serde_json::Value::Bool(false) => Ok(Some(PoolSni::Disabled)),
        serde_json::Value::String(s) => {
            let s = s.trim();
            Ok((!s.is_empty()).then(|| PoolSni::Name(s.to_string())))
        }
        other => Err(serde::de::Error::custom(format!(
            "invalid sni value {}; expected bool or string",
            other
        ))),
    }
}

fn deserialize_keepalive<'de, D>(deserializer: D) -> std::result::Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    let Some(value) = value else {
        return Ok(false);
    };

    match value {
        serde_json::Value::Null => Ok(false),
        serde_json::Value::Bool(v) => Ok(v),
        serde_json::Value::Number(n) => Ok(n.as_i64().unwrap_or_default() > 0),
        serde_json::Value::String(s) => Ok(matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )),
        other => Err(serde::de::Error::custom(format!(
            "invalid keepalive value {}; expected bool or number",
            other
        ))),
    }
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct CpuConfig {
    #[serde(default)]
    pub enabled: Option<bool>,
    pub threads: Option<u32>,
    #[serde(default)]
    #[serde(alias = "max-threads-hint")]
    pub max_threads_hint: Option<u32>,
    #[serde(default, rename = "rx/0", alias = "rx")]
    pub rx: Option<serde_json::Value>,
    #[serde(default)]
    pub priority: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct ApiConfig {
    pub bind: Option<SocketAddr>,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct HttpConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
}

fn default_log_level() -> String {
    "info".to_string()
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct FileConfig {
    #[serde(default)]
    pub pools: Option<Vec<PoolConfig>>,

    #[serde(default)]
    pub pool: Option<PoolConfig>,

    #[serde(default)]
    pub cpu: Option<CpuConfig>,

    #[serde(default)]
    pub api: Option<ApiConfig>,

    #[serde(default)]
    pub http: Option<HttpConfig>,

    #[serde(default)]
    pub logging: Option<LoggingConfig>,

    #[serde(default, alias = "user-agent", alias = "user_agent")]
    pub user_agent: Option<String>,

    #[serde(default)]
    pub background: Option<bool>,

    #[serde(default)]
    pub verbose: Option<u32>,

    #[serde(default)]
    pub use_e_cores: Option<bool>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Config {
    pub pool_url: String,
    pub pool_user: String,
    pub pool_pass: String,
    pub pool_tls: bool,
    pub pool_sni: Option<String>,
    pub user_agent: String,
    pub http2: bool,
    pub http3: bool,
    pub ws: bool,
    pub threads: u32,
    pub log_level: String,
    pub api_bind: Option<SocketAddr>,
    pub keepalive: bool,
    pub doh: bool,
    pub background: bool,
    pub use_e_cores: bool,
}

fn first_non_empty(a: Option<String>, b: Option<String>) -> Option<String> {
    match a {
        Some(s) if !s.is_empty() => Some(s),
        _ => match b {
            Some(s) if !s.is_empty() => Some(s),
            _ => None,
        },
    }
}

fn load_file_config(path: &std::path::Path) -> anyhow::Result<Option<FileConfig>> {
    let content = std::fs::read_to_string(path)?;

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    let cfg: FileConfig = if ext.eq_ignore_ascii_case("json") {
        serde_json::from_str(&content)?
    } else {
        toml::from_str(&content)?
    };

    Ok(Some(cfg))
}

fn find_config_file(args_path: &Option<PathBuf>) -> anyhow::Result<Option<FileConfig>> {
    if let Some(path) = args_path {
        return load_file_config(path);
    }

    for name in &["config.toml", "config.json", "config.toml", "config.json"] {
        let path = std::path::Path::new(name);
        if path.exists() {
            return load_file_config(path);
        }
    }

    Ok(None)
}

impl Config {
    pub fn load(args: &Args) -> anyhow::Result<Self> {
        let file_cfg = find_config_file(&args.config)?;

        let pool = file_cfg.as_ref().and_then(selected_pool);

        let cpu = file_cfg.as_ref().and_then(|c| c.cpu.as_ref());
        let api = file_cfg.as_ref().and_then(|c| c.api.as_ref());
        let http = file_cfg.as_ref().and_then(|c| c.http.as_ref());
        let logging = file_cfg.as_ref().and_then(|c| c.logging.as_ref());

        let pool_url = first_non_empty(args.url.clone(), pool.and_then(|p| p.url.clone()))
            .ok_or_else(|| anyhow::anyhow!("missing pool url, use -o/--url/--pool or config"))?;
        let inferred_tls = url_implies_tls(&pool_url);
        let inferred_http3 = url_implies_http3(&pool_url);
        let inferred_ws = url_implies_ws(&pool_url);

        let pool_user = first_non_empty(args.user.clone(), pool.and_then(|p| p.user.clone()))
            .unwrap_or_else(|| "x".to_string());

        let pool_pass = args
            .pass
            .clone()
            .or_else(|| pool.and_then(|p| p.pass.clone()))
            .unwrap_or_else(|| "x".to_string());

        let log_level = if let Some(level) = args.log_level.clone() {
            level
        } else if args.verbose {
            "debug".to_string()
        } else if file_cfg
            .as_ref()
            .and_then(|c| c.verbose)
            .unwrap_or_default()
            > 0
        {
            "debug".to_string()
        } else {
            logging
                .map(|l| l.level.clone())
                .unwrap_or_else(|| "info".to_string())
        };

        let threads = resolve_threads(args.threads, cpu);

        let background = args.background
            || file_cfg
                .as_ref()
                .and_then(|c| c.background)
                .unwrap_or(false);
        let pool_sni = resolve_pool_sni(args.sni.clone(), pool, &pool_url);

        Ok(Config {
            pool_url,
            pool_user,
            pool_pass,
            pool_tls: args.tls || pool.map(|p| p.tls).unwrap_or(false) || inferred_tls,
            pool_sni,
            user_agent: resolve_user_agent(
                args.user_agent
                    .clone()
                    .or_else(|| pool.and_then(|p| p.ua.clone()))
                    .or_else(|| file_cfg.as_ref().and_then(|c| c.user_agent.clone())),
            )?,
            http2: args.http2 || pool.map(|p| p.http2).unwrap_or(false),
            http3: args.http3 || pool.map(|p| p.http3).unwrap_or(false) || inferred_http3,
            ws: args.ws || pool.map(|p| p.ws).unwrap_or(false) || inferred_ws,
            threads,
            log_level,
            api_bind: args
                .api_bind
                .or_else(|| api.and_then(|a| a.bind))
                .or_else(|| http_bind_addr(http)),
            keepalive: args.keepalive || pool.map(|p| p.keepalive).unwrap_or(false),
            doh: args.doh || pool.map(|p| p.doh).unwrap_or(false),
            background,
            use_e_cores: args.use_e_cores
                || file_cfg
                    .as_ref()
                    .and_then(|c| c.use_e_cores)
                    .unwrap_or(false),
        })
    }
}

fn selected_pool(file_cfg: &FileConfig) -> Option<&PoolConfig> {
    file_cfg
        .pool
        .as_ref()
        .filter(|p| p.enabled)
        .or_else(|| {
            file_cfg
                .pools
                .as_ref()
                .and_then(|ps| ps.iter().find(|p| p.enabled))
        })
        .or_else(|| file_cfg.pool.as_ref())
        .or_else(|| file_cfg.pools.as_ref().and_then(|ps| ps.first()))
}

fn resolve_pool_sni(
    cli_sni: Option<String>,
    pool: Option<&PoolConfig>,
    pool_url: &str,
) -> Option<String> {
    if let Some(sni) = cli_sni
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return Some(sni);
    }

    match pool.and_then(|p| p.sni.as_ref()) {
        Some(PoolSni::Name(name)) => Some(name.clone()),
        Some(PoolSni::Enabled) => extract_host_from_pool_url(pool_url),
        Some(PoolSni::Disabled) | None => None,
    }
}

fn resolve_threads(cli_threads: Option<u32>, cpu: Option<&CpuConfig>) -> u32 {
    if let Some(threads) = cli_threads {
        return threads;
    }

    let Some(cpu) = cpu else {
        return 0;
    };

    if let Some(threads) = cpu.threads {
        return threads;
    }

    if let Some(threads) = cpu.rx.as_ref().and_then(threads_from_xmrig_cpu_value) {
        return threads;
    }

    cpu.max_threads_hint
        .and_then(threads_from_max_threads_hint)
        .unwrap_or(0)
}

fn threads_from_xmrig_cpu_value(value: &serde_json::Value) -> Option<u32> {
    match value {
        serde_json::Value::Array(threads) => {
            let count = threads
                .iter()
                .filter(|v| !matches!(v, serde_json::Value::Bool(false) | serde_json::Value::Null))
                .count() as u32;
            (count > 0).then_some(count)
        }
        serde_json::Value::Object(map) => map
            .get("threads")
            .and_then(|v| v.as_u64())
            .and_then(|n| u32::try_from(n).ok()),
        _ => None,
    }
}

fn threads_from_max_threads_hint(hint: u32) -> Option<u32> {
    if hint == 0 || hint >= 100 {
        return None;
    }

    let logical = num_cpus::get().max(1) as u32;
    Some(((logical * hint).div_ceil(100)).max(1))
}

fn http_bind_addr(http: Option<&HttpConfig>) -> Option<SocketAddr> {
    let http = http?;
    if !http.enabled {
        return None;
    }

    let port = http.port?;
    if port == 0 {
        return None;
    }

    let host = http.host.as_deref().unwrap_or("127.0.0.1");
    let ip = match host {
        "localhost" => IpAddr::from([127, 0, 0, 1]),
        _ => host.parse::<IpAddr>().ok()?,
    };

    Some(SocketAddr::new(ip, port))
}

fn extract_host_from_pool_url(url: &str) -> Option<String> {
    let raw = url.trim();
    if raw.is_empty() {
        return None;
    }

    let rest = raw.split_once("://").map(|(_, rest)| rest).unwrap_or(raw);
    let authority = match rest.find(|c| matches!(c, '/' | '?' | '#')) {
        Some(idx) => &rest[..idx],
        None => rest,
    };

    if let Some(stripped) = authority.strip_prefix('[') {
        let end = stripped.find(']')?;
        let host = &stripped[..end];
        return (!host.is_empty()).then(|| host.to_string());
    }

    let host = authority.rsplit_once(':').map(|(host, _)| host)?;
    if host.is_empty() || host.contains(':') {
        return None;
    }

    Some(host.to_string())
}

fn url_implies_tls(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("stratum+ssl://")
        || lower.starts_with("stratum+tls://")
        || lower.starts_with("ssl://")
        || lower.starts_with("tls://")
        || lower.starts_with("https://")
        || lower.starts_with("wss://")
        || lower.starts_with("http3://")
        || lower.starts_with("h3://")
}

fn url_implies_http3(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http3://") || lower.starts_with("h3://")
}

fn url_implies_ws(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("ws://") || lower.starts_with("wss://")
}

fn resolve_user_agent(mode: Option<String>) -> anyhow::Result<String> {
    let raw = mode
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("default");
    let mode = raw.to_ascii_lowercase();

    let lite = format!("lite-xmr/{} rust/2022", APP_VERSION);
    let value = match mode.as_str() {
        "default" => "XMRig/6.26.0 (Windows NT 10.0; Win64; x64)".to_string(),
        "edge" => "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36 Edg/149.0.0.0".to_string(),
        "full" => format!(
            "XMRig/6.26.0 (Windows NT 10.0; Win64; x64) libuv/1.51.0 msvc/2022 {}",
            lite
        ),
        "xmrig" => "XMRig/6.26.0 (Windows NT 10.0; Win64; x64) libuv/1.51.0 msvc/2022".to_string(),
        "fast" => lite,
        "short" => format!("lite-xmr/{}", APP_VERSION),
        "sogo" => "Mozilla/5.0 (Windows NT 6.1; WOW64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/49.0.2623.221 Safari/537.36 SE 2.X MetaSr 1.0".to_string(),
        "ie11" => "Mozilla/5.0 (Windows NT 6.1; WOW64; Trident/7.0; rv:11.0) like Gecko".to_string(),
        _ => raw.to_string(),
    };

    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_args() -> Args {
        Args {
            url: None,
            user: None,
            pass: None,
            threads: None,
            tls: false,
            sni: None,
            user_agent: None,
            http2: false,
            http3: false,
            ws: false,
            config: None,
            log_level: None,
            api_bind: None,
            keepalive: false,
            doh: false,
            background: false,
            bench_seconds: None,
            use_e_cores: false,
            verbose: false,
        }
    }

    #[test]
    fn scheme_inference_covers_http3_and_ws_pre_support() {
        assert!(url_implies_tls("http3://proxy.example:443"));
        assert!(url_implies_tls("h3://proxy.example:443"));
        assert!(url_implies_tls("wss://proxy.example:443/ws"));
        assert!(!url_implies_tls("ws://proxy.example:80/ws"));

        assert!(url_implies_http3("http3://proxy.example:443"));
        assert!(url_implies_http3("h3://proxy.example:443"));
        assert!(!url_implies_http3("https://proxy.example:443"));

        assert!(url_implies_ws("ws://proxy.example:80/ws"));
        assert!(url_implies_ws("wss://proxy.example:443/ws"));
        assert!(!url_implies_ws("stratum+tls://proxy.example:443"));
    }

    #[test]
    fn normalizes_legacy_multi_char_short_args() {
        assert_eq!(normalize_legacy_arg("-tls".into()), OsString::from("--tls"));
        assert_eq!(normalize_legacy_arg("-ua".into()), OsString::from("--ua"));
        assert_eq!(
            normalize_legacy_arg("-ua=xmrig".into()),
            OsString::from("--ua=xmrig")
        );
        assert_eq!(normalize_legacy_arg("-V".into()), OsString::from("-V"));
    }

    #[test]
    fn loads_xmrig_style_config_json() {
        let path =
            std::env::temp_dir().join(format!("lite-xmr-xmrig-config-{}.json", std::process::id()));
        std::fs::write(
            &path,
            r#"{
                "http": {
                    "enabled": true,
                    "host": "127.0.0.1",
                    "port": 18080
                },
                "background": true,
                "verbose": 1,
                "user-agent": "Custom/XMRig-Compatible Agent",
                "cpu": {
                    "max-threads-hint": 25,
                    "rx/0": [0, 2, false, 4]
                },
                "pools": [
                    {
                        "url": "disabled.example:3333",
                        "user": "disabled",
                        "enabled": false
                    },
                    {
                        "algo": null,
                        "coin": null,
                        "url": "stratum+tls://auto.c3pool.org:33333",
                        "user": "wallet",
                        "pass": "worker",
                        "rig-id": null,
                        "nicehash": false,
                        "keepalive": 30,
                        "enabled": true,
                        "tls": true,
                        "sni": true,
                        "tls-fingerprint": null,
                        "daemon": false,
                        "socks5": null,
                        "self-select": null,
                        "submit-to-origin": false
                    }
                ]
            }"#,
        )
        .unwrap();

        let mut args = empty_args();
        args.config = Some(path.clone());
        let config = Config::load(&args).unwrap();
        let _ = std::fs::remove_file(path);

        assert_eq!(config.pool_url, "stratum+tls://auto.c3pool.org:33333");
        assert_eq!(config.pool_user, "wallet");
        assert_eq!(config.pool_pass, "worker");
        assert!(config.pool_tls);
        assert_eq!(config.pool_sni.as_deref(), Some("auto.c3pool.org"));
        assert_eq!(config.user_agent, "Custom/XMRig-Compatible Agent");
        assert!(config.keepalive);
        assert!(config.background);
        assert_eq!(config.threads, 3);
        assert_eq!(
            config.api_bind,
            Some(SocketAddr::from(([127, 0, 0, 1], 18080)))
        );
        assert_eq!(config.log_level, "debug");
    }

    #[test]
    fn supports_string_sni_override_in_pool_config() {
        let pool = PoolConfig {
            url: Some("203.0.113.10:443".to_string()),
            user: Some("wallet".to_string()),
            pass: None,
            enabled: true,
            tls: true,
            sni: Some(PoolSni::Name("proxy.example.com".to_string())),
            ua: None,
            http2: false,
            http3: false,
            ws: false,
            keepalive: false,
            doh: false,
        };

        assert_eq!(
            resolve_pool_sni(None, Some(&pool), "203.0.113.10:443").as_deref(),
            Some("proxy.example.com")
        );
    }
}
