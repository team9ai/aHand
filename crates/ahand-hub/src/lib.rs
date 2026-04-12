pub mod audit_writer;
pub mod auth;
pub mod bootstrap;
pub mod config;
pub mod events;
pub mod http;
pub mod output_stream;
pub mod s3;
pub mod state;
pub mod ws;

use axum::Router;

pub fn build_app(state: state::AppState) -> Router {
    http::router(state)
}
