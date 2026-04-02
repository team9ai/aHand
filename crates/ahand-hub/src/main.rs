use ahand_hub::{build_app, config::Config, state::AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let config = Config::from_env()?;
    let bind_addr = config.bind_addr.clone();
    let state = AppState::from_config(config).await?;
    let app = build_app(state.clone());
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;

    tracing::info!(bind_addr = %bind_addr, "ahand-hub listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    state.shutdown().await?;
    Ok(())
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
