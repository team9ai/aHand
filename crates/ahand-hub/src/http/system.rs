use ahand_hub_core::job::JobFilter;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use serde::Serialize;

use crate::auth::AuthContextExt;
use crate::http::api_error::{ApiError, ApiResult};
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

pub async fn sentry_smoke(headers: HeaderMap) -> ApiResult<Json<HealthResponse>> {
    let expected = std::env::var("AHAND_HUB_SENTRY_SMOKE_TOKEN")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let Some(expected) = expected else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "NOT_FOUND", "Not found"));
    };

    let actual = headers
        .get("x-ahand-sentry-smoke-token")
        .and_then(|value| value.to_str().ok());
    if actual != Some(expected.as_str()) {
        return Err(ApiError::forbidden());
    }

    tracing::error!(
        target: "ahand_hub_sentry_smoke",
        smoke = true,
        "aHand hub Sentry smoke test"
    );
    Ok(Json(HealthResponse { ok: true }))
}

pub async fn stats(
    auth: AuthContextExt,
    State(state): State<AppState>,
) -> ApiResult<Json<StatsResponse>> {
    auth.require_read_stats()?;
    let devices = state
        .device_manager
        .list_devices()
        .await
        .map_err(|_| ApiError::internal("Failed to list devices"))?;
    let running_jobs = state
        .jobs_store
        .list(JobFilter {
            status: Some(ahand_hub_core::job::JobStatus::Running),
            ..Default::default()
        })
        .await
        .map_err(|_| ApiError::internal("Failed to list jobs"))?;

    let online_devices = devices.iter().filter(|device| device.online).count();
    Ok(Json(StatsResponse {
        online_devices,
        offline_devices: devices.len().saturating_sub(online_devices),
        running_jobs: running_jobs.len(),
    }))
}
