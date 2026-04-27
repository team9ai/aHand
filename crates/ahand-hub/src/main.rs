use ahand_hub::{build_app, config::Config, state::AppState};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    tracing::info!(
        git_sha = std::env::var("GIT_SHA").as_deref().unwrap_or("unknown"),
        "ahand-hub starting"
    );

    let config = Config::from_env()?;
    let bind_addr = config.bind_addr.clone();

    tracing::info!(bind_addr = %bind_addr, "config loaded; connecting to backing services");
    let state = AppState::from_config(config).await?;

    tracing::info!("backing services connected; binding listener");
    let app = build_app(state.clone());
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;

    tracing::info!(bind_addr = %bind_addr, "ahand-hub listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    state.shutdown().await?;
    Ok(())
}

fn init_tracing() {
    let level = std::env::var("AHAND_HUB_LOG_LEVEL").unwrap_or_else(|_| "info".into());
    let filter = EnvFilter::try_new(&level).unwrap_or_else(|_| EnvFilter::new("info"));
    let format = std::env::var("AHAND_HUB_LOG_FORMAT").unwrap_or_default();

    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if format.eq_ignore_ascii_case("json") {
        builder.json().init();
    } else {
        builder.init();
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        let _ = signal.recv().await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
