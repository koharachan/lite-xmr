use pico_args::Arguments;
use serde::Deserialize;
use std::net::SocketAddr;
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
        let mut pargs = Arguments::from_env();

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

        let args = Args {
            url,
            user: pargs.opt_value_from_str(["-u", "--user"])?,
            pass: pargs.opt_value_from_str(["-p", "--pass"])?,
            threads: pargs.opt_value_from_str(["-t", "--threads"])?,
            tls: pargs.contains(["-tls", "--tls"]),
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
    pub url: String,
    pub user: String,
    #[serde(default = "default_pass")]
    pub pass: String,
    #[serde(default)]
    pub tls: bool,
    #[serde(default)]
    pub keepalive: bool,
    #[serde(default)]
    pub doh: bool,
}

fn default_pass() -> String {
    "x".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct CpuConfig {
    #[serde(default)]
    pub enabled: Option<bool>,
    pub threads: Option<u32>,
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
    pub logging: Option<LoggingConfig>,

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

        let pool = file_cfg.as_ref().and_then(|c| {
            c.pool
                .as_ref()
                .or_else(|| c.pools.as_ref().and_then(|ps| ps.first()))
        });

        let cpu = file_cfg.as_ref().and_then(|c| c.cpu.as_ref());
        let api = file_cfg.as_ref().and_then(|c| c.api.as_ref());
        let logging = file_cfg.as_ref().and_then(|c| c.logging.as_ref());

        let pool_url = first_non_empty(args.url.clone(), pool.map(|p| p.url.clone()))
            .ok_or_else(|| anyhow::anyhow!("missing pool url, use -o/--url/--pool or config"))?;
        let inferred_tls = url_implies_tls(&pool_url);

        let pool_user = first_non_empty(args.user.clone(), pool.map(|p| p.user.clone()))
            .ok_or_else(|| anyhow::anyhow!("missing pool user, use -u/--user or config"))?;

        let pool_pass = args
            .pass
            .clone()
            .or_else(|| pool.map(|p| p.pass.clone()))
            .unwrap_or_else(|| "x".to_string());

        let log_level = if let Some(level) = args.log_level.clone() {
            level
        } else if args.verbose {
            "debug".to_string()
        } else {
            logging
                .map(|l| l.level.clone())
                .unwrap_or_else(|| "info".to_string())
        };

        let threads = args
            .threads
            .or_else(|| cpu.and_then(|c| c.threads))
            .unwrap_or(0);

        let background = args.background
            || file_cfg
                .as_ref()
                .and_then(|c| c.background)
                .unwrap_or(false);

        Ok(Config {
            pool_url,
            pool_user,
            pool_pass,
            pool_tls: args.tls || pool.map(|p| p.tls).unwrap_or(false) || inferred_tls,
            threads,
            log_level,
            api_bind: args.api_bind.or_else(|| api.and_then(|a| a.bind)),
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

fn url_implies_tls(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("stratum+ssl://")
        || lower.starts_with("stratum+tls://")
        || lower.starts_with("ssl://")
        || lower.starts_with("tls://")
        || lower.starts_with("https://")
}
