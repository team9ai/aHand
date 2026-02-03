mod client;
mod config;
mod executor;
mod policy;
mod registry;
mod store;

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

    /// Maximum number of concurrent jobs
    #[arg(long, env = "AHAND_MAX_JOBS")]
    max_jobs: Option<usize>,

    /// Directory for trace logs and run artifacts
    #[arg(long, env = "AHAND_DATA_DIR")]
    data_dir: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let mut cfg = if let Some(path) = &args.config {
        config::Config::load(path)?
    } else {
        // Build a minimal config from CLI args.
        config::Config {
            server_url: args
                .url
                .clone()
                .unwrap_or_else(|| "ws://localhost:3000/ws".to_string()),
            device_id: None,
            max_concurrent_jobs: None,
            data_dir: None,
            policy: Default::default(),
        }
    };

    // CLI args override config file values.
    if let Some(url) = args.url {
        cfg.server_url = url;
    }
    if let Some(max_jobs) = args.max_jobs {
        cfg.max_concurrent_jobs = Some(max_jobs);
    }
    if let Some(data_dir) = args.data_dir {
        cfg.data_dir = Some(data_dir);
    }

    info!(
        server_url = %cfg.server_url,
        device_id = %cfg.device_id(),
        max_concurrent_jobs = cfg.max_concurrent_jobs.unwrap_or(8),
        "ahandd starting"
    );

    client::run(cfg).await
}
