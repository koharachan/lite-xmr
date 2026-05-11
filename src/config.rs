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
            tls: pargs.contains("--tls"),
            config: pargs.opt_value_from_str("--config")?,
            log_level: pargs.opt_value_from_str("--log-level")?,
            api_bind: pargs.opt_value_from_str("--api-bind")?,
            keepalive: pargs.contains("--keepalive"),
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
    println!("      --config <PATH>      配置文件路径");
    println!("      --log-level <LEVEL>  日志级别 (默认: info)");
    println!("      --api-bind <ADDR>    HTTP API 监听地址");
    println!("      --keepalive          保持连接活跃 (不挖矿)");
    println!("  -h, --help               显示帮助信息");
    println!("  -v, --version            显示版本号");
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct TomlConfig {
    #[serde(default)]
    pub pool: Option<TomlPool>,

    #[serde(default)]
    pub cpu: Option<TomlCpu>,

    #[serde(default)]
    pub api: Option<TomlApi>,

    #[serde(default)]
    pub logging: Option<TomlLogging>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TomlPool {
    pub url: String,
    pub user: String,
    #[serde(default = "default_pass")]
    pub pass: String,
    #[serde(default)]
    pub tls: bool,
    #[serde(default)]
    pub keepalive: bool,
}

fn default_pass() -> String {
    "x".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct TomlCpu {
    pub threads: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TomlApi {
    pub bind: SocketAddr,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TomlLogging {
    #[serde(default = "default_log_level")]
    pub level: String,
}

fn default_log_level() -> String {
    "info".to_string()
}

#[derive(Debug, Clone)]
pub struct Config {
    pub pool_url: String,
    pub pool_user: String,
    pub pool_pass: String,
    pub pool_tls: bool,
    pub threads: u32,
    pub log_level: String,
    pub api_bind: Option<SocketAddr>,
    pub keepalive: bool,
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

impl Config {
    pub fn load(args: &Args) -> anyhow::Result<Self> {
        let toml_cfg = if let Some(ref path) = args.config {
            let content = std::fs::read_to_string(path)?;
            Some(toml::from_str::<TomlConfig>(&content)?)
        } else if let Ok(content) = std::fs::read_to_string("config.toml") {
            Some(toml::from_str::<TomlConfig>(&content)?)
        } else {
            None
        };

        let pool = toml_cfg.as_ref().and_then(|c| c.pool.as_ref());
        let cpu = toml_cfg.as_ref().and_then(|c| c.cpu.as_ref());
        let api = toml_cfg.as_ref().and_then(|c| c.api.as_ref());
        let logging = toml_cfg.as_ref().and_then(|c| c.logging.as_ref());

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

        Ok(Config {
            pool_url,
            pool_user,
            pool_pass,
            pool_tls: args.tls || pool.map(|p| p.tls).unwrap_or(false),
            threads,
            log_level,
            api_bind: args.api_bind.or(api.map(|a| a.bind)),
            keepalive: args.keepalive || pool.map(|p| p.keepalive).unwrap_or(false),
        })
    }
}
