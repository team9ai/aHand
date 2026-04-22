pub mod audit_writer;
pub mod auth;
pub mod bootstrap;
pub mod config;
pub mod control_jobs;
pub mod events;
pub mod http;
pub mod output_stream;
pub mod state;
pub mod ws;

use axum::Router;

pub fn build_app(state: state::AppState) -> Router {
    http::router(state)
}
