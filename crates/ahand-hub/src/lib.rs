pub mod audit_writer;
pub mod auth;
pub mod config;
pub mod events;
pub mod http;
pub mod output_stream;
pub mod state;
pub mod ws;

use axum::Router;

pub fn build_app(state: state::AppState) -> Router {
    http::router(state)
}

pub async fn build_test_app() -> Router {
    let state = state::AppState::for_tests().await;
    build_app(state)
}
