use ahand_hub::{build_app, config::Config, state::AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let config = Config::from_env();
    let bind_addr = config.bind_addr.clone();
    let state = AppState::from_config(config).await;
    let app = build_app(state);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;

    tracing::info!(bind_addr = %bind_addr, "ahand-hub listening");
    axum::serve(listener, app).await?;
    Ok(())
}
