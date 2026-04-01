use axum::{
    Router,
    routing::{get, post},
};

use crate::state::AppState;

pub mod devices;
pub mod jobs;
pub mod system;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(system::health))
        .route("/api/devices", get(devices::list_devices))
        .route("/api/jobs", post(jobs::create_job))
        .route("/api/jobs/{job_id}/cancel", post(jobs::cancel_job))
        .route("/api/jobs/{job_id}/output", get(jobs::stream_output))
        .route("/ws", get(crate::ws::device_gateway::handle_device_socket))
        .with_state(state)
}
