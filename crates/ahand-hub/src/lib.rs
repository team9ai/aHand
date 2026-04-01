pub mod auth;
pub mod config;
pub mod http;
pub mod state;

use axum::Router;

pub fn build_app(state: state::AppState) -> Router {
    http::router(state)
}

pub async fn build_test_app() -> Router {
    let state = state::AppState::for_tests().await;
    build_app(state)
}
