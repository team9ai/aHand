mod ahand_client;
mod approval;
mod browser;
mod config;
mod executor;
mod ipc;
mod openclaw;
mod outbox;
mod policy;
mod registry;
mod session;
mod store;

use std::path::PathBuf;
use std::sync::Arc;

use ahand_protocol::Envelope;
use clap::Parser;
use config::ConnectionMode;
use tracing::info;

#[derive(Parser)]
#[command(name = "ahandd", about = "AHand local execution daemon")]
struct Args {
    /// Connection mode: "ahand-cloud" (default) or "openclaw-gateway"
    #[arg(long, env = "AHAND_MODE")]
    mode: Option<String>,

    /// Cloud WebSocket URL (e.g. ws://localhost:3000/ws) - for ahand-cloud mode
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

    // OpenClaw Gateway options
    /// OpenClaw Gateway host
    #[arg(long, env = "OPENCLAW_GATEWAY_HOST")]
    gateway_host: Option<String>,

    /// OpenClaw Gateway port
    #[arg(long, env = "OPENCLAW_GATEWAY_PORT")]
    gateway_port: Option<u16>,

    /// Use TLS for OpenClaw Gateway connection
    #[arg(long, env = "OPENCLAW_GATEWAY_TLS")]
    gateway_tls: bool,

    /// OpenClaw node ID
    #[arg(long, env = "OPENCLAW_NODE_ID")]
    node_id: Option<String>,

    /// OpenClaw node display name
    #[arg(long, env = "OPENCLAW_DISPLAY_NAME")]
    display_name: Option<String>,

    /// OpenClaw Gateway authentication token
    #[arg(long, env = "OPENCLAW_GATEWAY_TOKEN")]
    gateway_token: Option<String>,

    /// OpenClaw Gateway authentication password
    #[arg(long, env = "OPENCLAW_GATEWAY_PASSWORD")]
    gateway_password: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let config_path = args.config.clone();

    let mut cfg = if let Some(path) = &config_path {
        config::Config::load(path)?
    } else {
        // Build a minimal config from CLI args.
        config::Config {
            mode: args.mode.clone(),
            server_url: args
                .url
                .clone()
                .unwrap_or_else(|| "ws://localhost:3000/ws".to_string()),
            device_id: None,
            max_concurrent_jobs: None,
            data_dir: None,
            debug_ipc: None,
            ipc_socket_path: None,
            ipc_socket_mode: None,
            trust_timeout_mins: None,
            default_session_mode: None,
            policy: Default::default(),
            openclaw: None,
            browser: None,
        }
    };

    // CLI args override config file values.
    if let Some(mode) = args.mode {
        cfg.mode = Some(mode);
    }
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

    // OpenClaw args override config file values
    if args.gateway_host.is_some()
        || args.gateway_port.is_some()
        || args.gateway_tls
        || args.node_id.is_some()
        || args.display_name.is_some()
        || args.gateway_token.is_some()
        || args.gateway_password.is_some()
    {
        let mut oc = cfg.openclaw.take().unwrap_or_default();
        if let Some(host) = args.gateway_host {
            oc.gateway_host = Some(host);
        }
        if let Some(port) = args.gateway_port {
            oc.gateway_port = Some(port);
        }
        if args.gateway_tls {
            oc.gateway_tls = Some(true);
        }
        if let Some(node_id) = args.node_id {
            oc.node_id = Some(node_id);
        }
        if let Some(display_name) = args.display_name {
            oc.display_name = Some(display_name);
        }
        if let Some(token) = args.gateway_token {
            oc.auth_token = Some(token);
        }
        if let Some(password) = args.gateway_password {
            oc.auth_password = Some(password);
        }
        cfg.openclaw = Some(oc);
    }

    let connection_mode = cfg.connection_mode();
    let device_id = cfg.device_id();
    let debug_ipc = cfg.debug_ipc.unwrap_or(false);
    let ipc_socket_path = cfg.ipc_socket_path();
    let ipc_socket_mode = cfg.ipc_socket_mode();

    // Shared resources.
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

    let session_mgr = Arc::new(session::SessionManager::new(
        cfg.trust_timeout_mins.unwrap_or(60),
    ));

    // Apply default session mode from config.
    if let Some(mode_str) = &cfg.default_session_mode {
        let mode = match mode_str.as_str() {
            "auto_accept" | "auto" => ahand_protocol::SessionMode::AutoAccept,
            "trust" => ahand_protocol::SessionMode::Trust,
            "strict" => ahand_protocol::SessionMode::Strict,
            _ => ahand_protocol::SessionMode::Inactive,
        };
        session_mgr.set_default_mode(mode).await;
    }

    let approval_mgr = Arc::new(approval::ApprovalManager::new(
        cfg.policy.approval_timeout_secs,
    ));

    // PolicyChecker preserved for future Mode 5 (preset) use.
    let _policy = Arc::new(policy::PolicyChecker::new(&cfg.policy));

    let browser_mgr = Arc::new(browser::BrowserManager::new(cfg.browser_config()));

    // Broadcast channel for pushing approval requests to all IPC clients.
    let (approval_broadcast_tx, _) = tokio::sync::broadcast::channel::<Envelope>(64);

    match connection_mode {
        ConnectionMode::AHandCloud => {
            info!(
                server_url = %cfg.server_url,
                device_id = %device_id,
                max_concurrent_jobs = max_jobs,
                debug_ipc,
                "ahandd starting in ahand-cloud mode"
            );

            if debug_ipc {
                let ipc_handle = tokio::spawn(ipc::serve_ipc(
                    ipc_socket_path,
                    ipc_socket_mode,
                    Arc::clone(&registry),
                    store_opt.clone(),
                    Arc::clone(&session_mgr),
                    Arc::clone(&approval_mgr),
                    approval_broadcast_tx.clone(),
                    device_id.clone(),
                    Arc::clone(&browser_mgr),
                ));

                // Run WS client and IPC server concurrently.
                tokio::select! {
                    r = ahand_client::run(cfg, device_id, registry, store_opt, session_mgr, approval_mgr, approval_broadcast_tx, Arc::clone(&browser_mgr)) => r,
                    r = ipc_handle => {
                        r??;
                        Ok(())
                    }
                }
            } else {
                ahand_client::run(
                    cfg,
                    device_id,
                    registry,
                    store_opt,
                    session_mgr,
                    approval_mgr,
                    approval_broadcast_tx,
                    browser_mgr,
                )
                .await
            }
        }
        ConnectionMode::OpenClawGateway => {
            let oc_config = cfg.openclaw_config();
            let host = oc_config.gateway_host.as_deref().unwrap_or("127.0.0.1");
            let port = oc_config.gateway_port.unwrap_or(18789);

            info!(
                gateway_host = %host,
                gateway_port = port,
                node_id = ?oc_config.node_id,
                display_name = ?oc_config.display_name,
                max_concurrent_jobs = max_jobs,
                debug_ipc,
                "ahandd starting in openclaw-gateway mode"
            );

            let client = openclaw::OpenClawClient::new(
                oc_config,
                Arc::clone(&registry),
                Arc::clone(&session_mgr),
                Arc::clone(&approval_mgr),
                store_opt.clone(),
                Arc::clone(&browser_mgr),
            );

            if debug_ipc {
                let ipc_handle = tokio::spawn(ipc::serve_ipc(
                    ipc_socket_path,
                    ipc_socket_mode,
                    Arc::clone(&registry),
                    store_opt.clone(),
                    Arc::clone(&session_mgr),
                    Arc::clone(&approval_mgr),
                    approval_broadcast_tx.clone(),
                    device_id.clone(),
                    Arc::clone(&browser_mgr),
                ));

                // Run OpenClaw client and IPC server concurrently.
                tokio::select! {
                    r = client.run() => r,
                    r = ipc_handle => {
                        r??;
                        Ok(())
                    }
                }
            } else {
                client.run().await
            }
        }
    }
}
