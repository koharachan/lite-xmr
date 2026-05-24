mod app;
mod config;
mod controller;
mod cpu;
mod doh;
mod error;
mod job;
mod miner;
mod randomx;
mod stats;
mod stratum;
mod taskbar;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    app::run().await
}
