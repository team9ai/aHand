use ahand_hub_core::device::Device;
use ahand_hub_core::traits::DeviceStore;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};

use crate::auth::AuthContextExt;
use crate::state::AppState;

pub async fn list_devices(
    auth: AuthContextExt,
    State(state): State<AppState>,
) -> Result<Json<Vec<Device>>, StatusCode> {
    auth.require_read_devices()?;
    let devices = state
        .device_manager
        .list_devices()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(devices))
}

pub async fn get_device(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Path(device_id): Path<String>,
) -> Result<Json<Device>, StatusCode> {
    auth.require_read_devices()?;
    let device = state
        .devices
        .get(&device_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(device))
}
