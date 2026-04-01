use ahand_hub_core::job::JobFilter;
use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;

use crate::auth::AuthContextExt;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub ok: bool,
}

#[derive(Debug, Serialize)]
pub struct StatsResponse {
    pub online_devices: usize,
    pub offline_devices: usize,
    pub running_jobs: usize,
}

pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}

pub async fn stats(
    auth: AuthContextExt,
    State(state): State<AppState>,
) -> Result<Json<StatsResponse>, StatusCode> {
    auth.require_read_stats()?;
    let devices = state
        .device_manager
        .list_devices()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let running_jobs = state
        .jobs_store
        .list(JobFilter {
            status: Some(ahand_hub_core::job::JobStatus::Running),
            ..Default::default()
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let online_devices = devices.iter().filter(|device| device.online).count();
    Ok(Json(StatsResponse {
        online_devices,
        offline_devices: devices.len().saturating_sub(online_devices),
        running_jobs: running_jobs.len(),
    }))
}
