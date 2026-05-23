mod config;
mod controller;
mod cpu;
mod doh;
mod error;
mod job;
mod miner;
mod stats;
mod stratum;
mod taskbar;

use std::process;
use tracing::info;
use tracing_subscriber::fmt::{format::Writer, time::FormatTime};

use config::{Args, Config, EarlyExit};
use controller::Controller;

struct LocalLogTime;

impl FormatTime for LocalLogTime {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", chrono::Local::now().format("%y/%m/%d %H:%M:%S"))
    }
}

fn daemonize() {
    #[cfg(target_family = "unix")]
    {
        if unsafe { libc::fork() } != 0 {
            process::exit(0);
        }
        unsafe {
            libc::setsid();
        }
        if unsafe { libc::fork() } != 0 {
            process::exit(0);
        }
        unsafe {
            libc::chdir(b"/\0".as_ptr() as *const _);
        }
        let devnull = unsafe { libc::open(b"/dev/null\0".as_ptr() as *const _, libc::O_RDWR) };
        if devnull >= 0 {
            unsafe {
                libc::dup2(devnull, libc::STDIN_FILENO);
                libc::dup2(devnull, libc::STDOUT_FILENO);
                libc::dup2(devnull, libc::STDERR_FILENO);
                libc::close(devnull);
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Install rustls' ring crypto provider before any TLS connection is made.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let args = match Args::parse()? {
        Ok(a) => a,
        Err(EarlyExit::Help) | Err(EarlyExit::Version) => return Ok(()),
    };
    let config = Config::load(&args)?;

    if config.background {
        daemonize();
    }

    tracing_subscriber::fmt()
        .compact()
        .with_timer(LocalLogTime)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| config.log_level.clone().parse().unwrap()),
        )
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .init();

    let mut controller = Controller::new();
    if let Err(e) = controller.run(&config).await {
        info!("miner stopped: {}", e);
        process::exit(1);
    }

    Ok(())
}
