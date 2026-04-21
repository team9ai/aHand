use ahand_hub_core::device::Device;
use ahand_hub_core::device::NewDevice;
use ahand_hub_core::traits::DeviceStore;
use axum::{
    Json,
    extract::rejection::JsonRejection,
    extract::rejection::QueryRejection,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};

use crate::auth::AuthContextExt;
use crate::http::api_error::{ApiError, ApiResult};
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

#[derive(Debug, Deserialize, Default)]
pub struct DeviceListQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

pub async fn list_devices(
    auth: AuthContextExt,
    State(state): State<AppState>,
    query: Result<Query<DeviceListQuery>, QueryRejection>,
) -> ApiResult<Json<Vec<Device>>> {
    auth.require_read_devices()?;
    let Query(query) = query.map_err(ApiError::from_query_rejection)?;
    let mut devices = state
        .device_manager
        .list_devices()
        .await
        .map_err(|_| ApiError::internal("Failed to list devices"))?;
    apply_pagination(&mut devices, query.offset.unwrap_or(0), query.limit);
    Ok(Json(devices))
}

pub async fn create_device(
    auth: AuthContextExt,
    State(state): State<AppState>,
    body: Result<Json<CreateDeviceRequest>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<CreateDeviceResponse>)> {
    auth.require_admin()?;
    let Json(body) = body.map_err(ApiError::from_json_rejection)?;
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
            external_user_id: None,
        })
        .await
        .map_err(ApiError::from)?;
    state
        .devices
        .mark_offline(&body.id)
        .await
        .map_err(ApiError::from)?;

    let bootstrap_token = match state.bootstrap_tokens.issue(&body.id).await {
        Ok(token) => token,
        Err(_) => {
            let _ = state.devices.delete(&body.id).await;
            return Err(ApiError::internal("Failed to issue bootstrap token"));
        }
    };

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
) -> ApiResult<Json<Device>> {
    auth.require_read_device(&device_id)?;
    let device = state
        .devices
        .get(&device_id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "DEVICE_NOT_FOUND",
                format!("Device {device_id} was not found"),
            )
        })?;
    Ok(Json(device))
}

pub async fn get_device_capabilities(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Path(device_id): Path<String>,
) -> ApiResult<Json<DeviceCapabilitiesResponse>> {
    auth.require_read_device(&device_id)?;
    let device = state
        .devices
        .get(&device_id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "DEVICE_NOT_FOUND",
                format!("Device {device_id} was not found"),
            )
        })?;
    Ok(Json(DeviceCapabilitiesResponse {
        device_id,
        capabilities: device.capabilities,
    }))
}

pub async fn delete_device(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Path(device_id): Path<String>,
) -> ApiResult<StatusCode> {
    auth.require_admin()?;
    state
        .devices
        .delete(&device_id)
        .await
        .map_err(ApiError::from)?;
    let _ = state.bootstrap_tokens.delete_device(&device_id).await;
    state
        .append_audit_entry(
            "device.deleted",
            "device",
            &device_id,
            &auth.0.subject,
            serde_json::json!({}),
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

fn apply_pagination<T>(items: &mut Vec<T>, offset: usize, limit: Option<usize>) {
    if offset == 0 && limit.is_none() {
        return;
    }

    let take = limit.unwrap_or(usize::MAX);
    let paged = std::mem::take(items)
        .into_iter()
        .skip(offset)
        .take(take)
        .collect();
    *items = paged;
}
