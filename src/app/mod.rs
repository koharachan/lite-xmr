mod benchmark;
mod daemon;
mod logging;

use std::process;

use tracing::info;

use crate::config::{Args, Config, EarlyExit};
use crate::controller::Controller;

pub async fn run() -> anyhow::Result<()> {
    // Install rustls' ring crypto provider before any TLS connection is made.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let args = match Args::parse()? {
        Ok(a) => a,
        Err(EarlyExit::Help) | Err(EarlyExit::Version) => return Ok(()),
    };

    if args.bench_seconds.is_some() {
        logging::init(args.log_level.as_deref().unwrap_or("info"));
        benchmark::run(&args)?;
        return Ok(());
    }

    let config = Config::load(&args)?;

    if config.background {
        daemon::daemonize();
    }

    logging::init(&config.log_level);

    let mut controller = Controller::new();
    if let Err(e) = controller.run(&config).await {
        info!("miner stopped: {}", e);
        process::exit(1);
    }

    Ok(())
}
