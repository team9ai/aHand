mod client;
mod config;
mod executor;
mod ipc;
mod outbox;
mod policy;
mod registry;
mod store;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
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

    /// Enable debug IPC server (Unix socket)
    #[arg(long, env = "AHAND_DEBUG_IPC")]
    debug_ipc: bool,

    /// Custom path for the IPC Unix socket
    #[arg(long, env = "AHAND_IPC_SOCKET")]
    ipc_socket: Option<String>,
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
            debug_ipc: None,
            ipc_socket_path: None,
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
    if args.debug_ipc {
        cfg.debug_ipc = Some(true);
    }
    if let Some(ipc_socket) = args.ipc_socket {
        cfg.ipc_socket_path = Some(ipc_socket);
    }

    let device_id = cfg.device_id();
    let debug_ipc = cfg.debug_ipc.unwrap_or(false);
    let ipc_socket_path = cfg.ipc_socket_path();

    info!(
        server_url = %cfg.server_url,
        device_id = %device_id,
        max_concurrent_jobs = cfg.max_concurrent_jobs.unwrap_or(8),
        debug_ipc,
        "ahandd starting"
    );

    // Shared resources: registry, store, policy.
    let max_jobs = cfg.max_concurrent_jobs.unwrap_or(8);
    let registry = Arc::new(registry::JobRegistry::new(max_jobs));

    let store_opt = match cfg.data_dir() {
        Some(dir) => match store::RunStore::new(&dir) {
            Ok(s) => {
                info!(data_dir = %dir.display(), "run store initialised");
                Some(Arc::new(s))
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to initialise run store, persistence disabled");
                None
            }
        },
        None => None,
    };

    let policy = Arc::new(policy::PolicyChecker::new(&cfg.policy));

    if debug_ipc {
        let ipc_handle = tokio::spawn(ipc::serve_ipc(
            ipc_socket_path,
            Arc::clone(&registry),
            store_opt.clone(),
            Arc::clone(&policy),
            device_id.clone(),
        ));

        // Run WS client and IPC server concurrently.
        tokio::select! {
            r = client::run(cfg, device_id, registry, store_opt, policy) => r,
            r = ipc_handle => {
                r??;
                Ok(())
            }
        }
    } else {
        client::run(cfg, device_id, registry, store_opt, policy).await
    }
}
