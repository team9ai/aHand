mod client;
mod config;
mod executor;
mod policy;

use clap::Parser;
use std::path::PathBuf;
use tracing::info;

#[derive(Parser)]
#[command(name = "ahandd", about = "AHand local execution daemon")]
struct Args {
    /// Cloud WebSocket URL (e.g. ws://localhost:3000/ws)
    #[arg(long, env = "AHAND_URL")]
    url: Option<String>,

    /// Path to config file (TOML)
    #[arg(long, short, env = "AHAND_CONFIG")]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let cfg = if let Some(path) = &args.config {
        config::Config::load(path)?
    } else {
        // Build a minimal config from CLI args.
        config::Config {
            server_url: args
                .url
                .unwrap_or_else(|| "ws://localhost:3000/ws".to_string()),
            device_id: None,
            policy: Default::default(),
        }
    };

    info!(
        server_url = %cfg.server_url,
        device_id = %cfg.device_id(),
        "ahandd starting"
    );

    client::run(cfg).await
}
