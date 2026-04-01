use ahand_hub_core::device::Device;
use axum::{Json, extract::State, http::StatusCode};

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
