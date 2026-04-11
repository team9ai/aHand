use axum::{
    Router,
    routing::{get, post},
};

use crate::state::AppState;

pub mod api_error;
pub mod audit;
pub mod auth;
pub mod browser;
pub mod devices;
pub mod jobs;
pub mod system;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(system::health))
        .route("/api/stats", get(system::stats))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/verify", get(auth::verify))
        .route(
            "/api/devices",
            get(devices::list_devices).post(devices::create_device),
        )
        .route(
            "/api/devices/{device_id}",
            get(devices::get_device).delete(devices::delete_device),
        )
        .route(
            "/api/devices/{device_id}/capabilities",
            get(devices::get_device_capabilities),
        )
        .route("/api/jobs", get(jobs::list_jobs).post(jobs::create_job))
        .route("/api/jobs/{job_id}", get(jobs::get_job))
        .route("/api/jobs/{job_id}/cancel", post(jobs::cancel_job))
        .route("/api/jobs/{job_id}/stdin", post(jobs::send_stdin))
        .route("/api/jobs/{job_id}/resize", post(jobs::send_resize))
        .route("/api/jobs/{job_id}/output", get(jobs::stream_output))
        .route("/api/audit-logs", get(audit::list_audit_logs))
        .route("/api/browser", post(browser::browser_command))
        .route("/ws", get(crate::ws::device_gateway::handle_device_socket))
        .route(
            "/ws/dashboard",
            get(crate::ws::dashboard::handle_dashboard_socket),
        )
        .with_state(state)
}
