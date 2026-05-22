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

        let args = Args {
            url: pargs.opt_value_from_str(["-o", "--url"])?,
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
        };

        let remaining = pargs.finish();
        if !remaining.is_empty() {
            anyhow::bail!("未知参数: {:?}", remaining);
        }

        Ok(Ok(args))
    }
}

fn print_usage() {
    println!("lite-xmr v{} - 轻量级 Monero (XMR) CPU 矿工", APP_VERSION);
    println!();
    println!("用法: lite-xmr [选项]");
    println!();
    println!("选项:");
    println!("  -o, --url <HOST:PORT>    矿池地址");
    println!("  -u, --user <ADDRESS>     钱包地址或用户名");
    println!("  -p, --pass <STRING>      矿池密码 (默认: x)");
    println!("  -t, --threads <N>        挖矿线程数 (0 = 自动检测)");
    println!("      --tls                使用 TLS 连接矿池");
    println!("      --config <PATH>      配置文件路径 (支持 .json/.toml)");
    println!("      --log-level <LEVEL>  日志级别 (默认: info)");
    println!("      --api-bind <ADDR>    HTTP API 监听地址");
    println!("  -k, --keepalive          保持连接活跃 (不挖矿)");
    println!("      --doh                使用 DNS over HTTPS 解析矿池地址");
    println!("  -B, --background         后台运行 (守护进程模式)");
    println!("  -h, --help               显示帮助信息");
    println!("  -v, --version            显示版本号");
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

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

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

        let pool = file_cfg
            .as_ref()
            .and_then(|c| {
                c.pool.as_ref().or_else(|| {
                    c.pools.as_ref().and_then(|ps| ps.first())
                })
            });

        let cpu = file_cfg.as_ref().and_then(|c| c.cpu.as_ref());
        let api = file_cfg.as_ref().and_then(|c| c.api.as_ref());
        let logging = file_cfg.as_ref().and_then(|c| c.logging.as_ref());

        let pool_url = first_non_empty(
            args.url.clone(),
            pool.map(|p| p.url.clone()),
        )
        .ok_or_else(|| anyhow::anyhow!("未指定矿池地址，请使用 -o/--url 或配置文件"))?;

        let pool_user = first_non_empty(
            args.user.clone(),
            pool.map(|p| p.user.clone()),
        )
        .ok_or_else(|| anyhow::anyhow!("未指定钱包地址，请使用 -u/--user 或配置文件"))?;

        let pool_pass = args
            .pass
            .clone()
            .or_else(|| pool.map(|p| p.pass.clone()))
            .unwrap_or_else(|| "x".to_string());

        let log_level = args
            .log_level
            .clone()
            .or_else(|| logging.map(|l| l.level.clone()))
            .unwrap_or_else(|| "info".to_string());

        let threads = args
            .threads
            .or_else(|| cpu.and_then(|c| c.threads))
            .unwrap_or(0);

        let background = args.background
            || file_cfg.as_ref().and_then(|c| c.background).unwrap_or(false);

        Ok(Config {
            pool_url,
            pool_user,
            pool_pass,
            pool_tls: args.tls || pool.map(|p| p.tls).unwrap_or(false),
            threads,
            log_level,
            api_bind: args
                .api_bind
                .or_else(|| api.and_then(|a| a.bind)),
            keepalive: args.keepalive || pool.map(|p| p.keepalive).unwrap_or(false),
            doh: args.doh || pool.map(|p| p.doh).unwrap_or(false),
            background,
        })
    }
}
