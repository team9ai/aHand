use ahand_hub_core::device::Device;
use ahand_hub_core::device::NewDevice;
use ahand_hub_core::traits::DeviceStore;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};

use crate::auth::AuthContextExt;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct CreateDeviceRequest {
    pub id: String,
    pub hostname: String,
    pub os: String,
    pub capabilities: Vec<String>,
    pub version: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateDeviceResponse {
    pub device_id: String,
    pub bootstrap_token: String,
}

#[derive(Debug, Serialize)]
pub struct DeviceCapabilitiesResponse {
    pub device_id: String,
    pub capabilities: Vec<String>,
}

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

pub async fn create_device(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Json(body): Json<CreateDeviceRequest>,
) -> Result<(StatusCode, Json<CreateDeviceResponse>), StatusCode> {
    auth.require_admin()?;
    state
        .devices
        .insert(NewDevice {
            id: body.id.clone(),
            public_key: None,
            hostname: body.hostname,
            os: body.os,
            capabilities: body.capabilities,
            version: body.version,
            auth_method: "bootstrap".into(),
        })
        .await
        .map_err(|err| match err {
            ahand_hub_core::HubError::DeviceAlreadyExists(_) => StatusCode::CONFLICT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        })?;
    state
        .devices
        .mark_offline(&body.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let bootstrap_token = state
        .auth
        .issue_device_jwt(&body.id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok((
        StatusCode::CREATED,
        Json(CreateDeviceResponse {
            device_id: body.id,
            bootstrap_token,
        }),
    ))
}

pub async fn get_device(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Path(device_id): Path<String>,
) -> Result<Json<Device>, StatusCode> {
    auth.require_read_device(&device_id)?;
    let device = state
        .devices
        .get(&device_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(device))
}

pub async fn get_device_capabilities(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Path(device_id): Path<String>,
) -> Result<Json<DeviceCapabilitiesResponse>, StatusCode> {
    auth.require_read_device(&device_id)?;
    let device = state
        .devices
        .get(&device_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(DeviceCapabilitiesResponse {
        device_id,
        capabilities: device.capabilities,
    }))
}

pub async fn delete_device(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Path(device_id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    auth.require_admin()?;
    state
        .devices
        .delete(&device_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}
