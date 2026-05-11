//! 挖矿配置定义。

use pico_args::Arguments;
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::PathBuf;

/// lite-xmr 命令行参数。
#[derive(Debug, Clone)]
pub struct Args {
    /// 矿池地址 (host:port)
    pub url: Option<String>,

    /// 钱包地址或用户名
    pub user: Option<String>,

    /// 矿池密码
    pub pass: String,

    /// 挖矿线程数 (0 = 自动检测)
    pub threads: u32,

    /// 使用 TLS 连接矿池
    pub tls: bool,

    /// 配置文件路径
    pub config: Option<PathBuf>,

    /// 日志级别 (trace, debug, info, warn, error)
    pub log_level: String,

    /// HTTP API 监听地址 (例如 127.0.0.1:8080)，不设置则不启用
    pub api_bind: Option<SocketAddr>,

    /// 保持连接活跃 (不实际挖矿，用于测试)
    pub keepalive: bool,
}

impl Args {
    /// 从命令行参数解析。
    pub fn parse() -> anyhow::Result<Self> {
        let mut pargs = Arguments::from_env();

        // 帮助和版本
        if pargs.contains("-h") || pargs.contains("--help") {
            print_usage();
            std::process::exit(0);
        }
        if pargs.contains("-v") || pargs.contains("--version") {
            println!("lite-xmr v0.1.0");
            std::process::exit(0);
        }

        Ok(Args {
            url: pargs.opt_value_from_str(["-o", "--url"])?,
            user: pargs.opt_value_from_str(["-u", "--user"])?,
            pass: pargs
                .opt_value_from_str(["-p", "--pass"])?
                .unwrap_or_else(|| "x".to_string()),
            threads: pargs
                .opt_value_from_str(["-t", "--threads"])?
                .unwrap_or(0),
            tls: pargs.contains("--tls"),
            config: pargs.opt_value_from_str("--config")?,
            log_level: pargs
                .opt_value_from_str("--log-level")?
                .unwrap_or_else(|| "info".to_string()),
            api_bind: pargs.opt_value_from_str("--api-bind")?,
            keepalive: pargs.contains("--keepalive"),
        })
    }
}

/// 打印使用帮助。
fn print_usage() {
    println!("lite-xmr v0.1.0 - 轻量级 Monero (XMR) CPU 矿工");
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

/// TOML 配置文件格式。
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
    #[serde(default)]
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

/// 运行时合并配置：命令行参数优先于配置文件。
#[derive(Debug, Clone)]
pub struct Config {
    /// 矿池 URL
    pub pool_url: String,

    /// 矿池用户名（钱包地址）
    pub pool_user: String,

    /// 矿池密码
    pub pool_pass: String,

    /// 是否使用 TLS
    pub pool_tls: bool,

    /// 挖矿线程数
    pub threads: u32,

    /// 日志级别
    pub log_level: String,

    /// HTTP API 监听地址
    pub api_bind: Option<SocketAddr>,

    /// 保持连接模式
    pub keepalive: bool,
}

impl Config {
    /// 从命令行参数和可选的配置文件构建运行时配置。
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
        let _logging = toml_cfg.as_ref().and_then(|c| c.logging.as_ref());

        let pool_url = args
            .url
            .clone()
            .or_else(|| pool.map(|p| p.url.clone()))
            .ok_or_else(|| anyhow::anyhow!("未指定矿池地址，请使用 -o/--url 或配置文件"))?;

        let pool_user = args
            .user
            .clone()
            .or_else(|| pool.map(|p| p.user.clone()))
            .ok_or_else(|| anyhow::anyhow!("未指定钱包地址，请使用 -u/--user 或配置文件"))?;

        Ok(Config {
            pool_url,
            pool_user,
            pool_pass: args.pass.clone(),
            pool_tls: args.tls || pool.map(|p| p.tls).unwrap_or(false),
            threads: if args.threads > 0 {
                args.threads
            } else {
                cpu.and_then(|c| c.threads).unwrap_or(0)
            },
            log_level: args.log_level.clone(),
            api_bind: args.api_bind.or(api.map(|a| a.bind)),
            keepalive: args.keepalive || pool.map(|p| p.keepalive).unwrap_or(false),
        })
    }
}
