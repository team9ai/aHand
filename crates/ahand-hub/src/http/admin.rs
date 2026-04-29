//! Admin API for team9 gateway: pre-register devices, mint device /
//! control-plane JWTs, revoke devices, list devices per external user.
//!
//! All endpoints require a valid service token in
//! `Authorization: Bearer <AHAND_HUB_SERVICE_TOKEN>`. The comparison is
//! constant-time to avoid trivial timing leaks.

use std::time::Duration;

use ahand_hub_core::HubError;
use ahand_hub_core::auth;
use ahand_hub_core::device::Device;
use ahand_hub_core::traits::DeviceAdminStore;
use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{StatusCode, header::AUTHORIZATION},
    middleware::{Next, from_fn_with_state},
    response::{IntoResponse, Response},
    routing::{delete as delete_method, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// Admin endpoints, all mounted under `/api/admin/*` and gated by
/// [`require_service_token`].
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/api/admin/devices", post(pre_register).get(list_devices))
        .route("/api/admin/devices/{id}", delete_method(delete_device))
        .route("/api/admin/devices/{id}/token", post(mint_device_token))
        .route(
            "/api/admin/control-plane/token",
            post(mint_control_plane_token),
        )
        .layer(from_fn_with_state(state, require_service_token))
}

async fn require_service_token(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, AdminError> {
    let Some(value) = req.headers().get(AUTHORIZATION) else {
        return Err(AdminError::Unauthorized);
    };
    let Ok(value) = value.to_str() else {
        return Err(AdminError::Unauthorized);
    };
    let Some(token) = value.strip_prefix("Bearer ") else {
        return Err(AdminError::Unauthorized);
    };
    if !service_token_matches(state.service_token.as_bytes(), token.as_bytes()) {
        return Err(AdminError::Unauthorized);
    }
    Ok(next.run(req).await)
}

fn service_token_matches(expected: &[u8], actual: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    expected.ct_eq(actual).into()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreRegisterRequest {
    pub device_id: String,
    /// Base64-encoded device public key (ed25519). Validated for non-empty
    /// decode only — we accept the bytes opaquely so rotating key formats
    /// upstream doesn't require a hub change.
    pub public_key: String,
    pub external_user_id: String,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreRegisterResponse {
    pub device_id: String,
    /// Stable `registered_at` timestamp from the DB row. Pre-register is
    /// idempotent: repeated calls for the same device return the original
    /// insertion timestamp, not the current server time.
    pub created_at: DateTime<Utc>,
}

async fn pre_register(
    State(state): State<AppState>,
    body: Result<Json<PreRegisterRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<PreRegisterResponse>, AdminError> {
    let Json(req) = body.map_err(|_| AdminError::BadRequest("invalid JSON body".into()))?;
    if req.device_id.is_empty() {
        return Err(AdminError::BadRequest("deviceId must not be empty".into()));
    }
    if req.external_user_id.is_empty() {
        return Err(AdminError::BadRequest(
            "externalUserId must not be empty".into(),
        ));
    }
    let public_key = decode_public_key(&req.public_key)?;
    let (device, registered_at) = state
        .devices
        .pre_register(&req.device_id, &public_key, &req.external_user_id)
        .await?;
    // Per spec § 2.2.4, `device.registered` fires when the service
    // token pre-registers a device. The WS hello-accept path also
    // emits this event for legacy self-registering devices that
    // bypassed the admin API; both are idempotent on the downstream
    // gateway because it dedupes by `eventId`.
    //
    // A failure to enqueue must not roll back the pre-register — the
    // row is durably inserted and the caller still gets a success
    // response. A dropped webhook is a dashboard nit, not a
    // correctness problem.
    if let Err(err) = state
        .webhook
        .enqueue_registered(&device.id, Some(&req.external_user_id), &[])
        .await
    {
        tracing::warn!(
            device_id = %device.id,
            error = %err,
            "failed to enqueue device.registered webhook after admin pre_register",
        );
    }
    Ok(Json(PreRegisterResponse {
        device_id: device.id,
        created_at: registered_at,
    }))
}

fn decode_public_key(value: &str) -> Result<Vec<u8>, AdminError> {
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;
    engine
        .decode(value)
        .ok()
        .filter(|bytes| !bytes.is_empty())
        .ok_or_else(|| AdminError::BadRequest("publicKey must be non-empty base64".into()))
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MintDeviceTokenRequest {
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenResponse {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceTokenResponse {
    pub token: String,
    pub device_id: String,
    pub external_user_id: String,
    pub expires_at: DateTime<Utc>,
}

async fn mint_device_token(
    State(state): State<AppState>,
    Path(device_id): Path<String>,
    body: Option<Json<MintDeviceTokenRequest>>,
) -> Result<Json<DeviceTokenResponse>, AdminError> {
    let req = body.map(|Json(v)| v).unwrap_or_default();
    if req.ttl_seconds == Some(0) {
        return Err(AdminError::BadRequest("ttlSeconds must be > 0".into()));
    }
    let device = state
        .devices
        .find_by_id(&device_id)
        .await?
        .ok_or(AdminError::NotFound)?;
    let external_user_id = device
        .external_user_id
        .clone()
        .ok_or(AdminError::NotFound)?;
    let ttl = req
        .ttl_seconds
        .map(Duration::from_secs)
        .unwrap_or(Duration::ZERO);
    let (token, expires_at) =
        state
            .auth
            .mint_device_jwt_with_external_user(&device.id, &external_user_id, ttl)?;
    Ok(Json(DeviceTokenResponse {
        token,
        device_id: device.id,
        external_user_id,
        expires_at,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MintControlPlaneRequest {
    pub external_user_id: String,
    #[serde(default)]
    pub device_ids: Option<Vec<String>>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlPlaneTokenResponse {
    pub token: String,
    pub external_user_id: String,
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_ids: Option<Vec<String>>,
    pub expires_at: DateTime<Utc>,
}

/// Scopes that the hub accepts for control-plane tokens. Anything
/// outside this list would mint a token that downstream endpoints
/// silently reject; we fail the mint request with 400 up-front
/// instead so callers see the problem immediately.
const VALID_CONTROL_PLANE_SCOPES: &[&str] = &["jobs:execute"];

async fn mint_control_plane_token(
    State(state): State<AppState>,
    body: Result<Json<MintControlPlaneRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<ControlPlaneTokenResponse>, AdminError> {
    let Json(req) = body.map_err(|_| AdminError::BadRequest("invalid JSON body".into()))?;
    if req.external_user_id.is_empty() {
        return Err(AdminError::BadRequest(
            "externalUserId must not be empty".into(),
        ));
    }
    if req.ttl_seconds == Some(0) {
        return Err(AdminError::BadRequest("ttlSeconds must be > 0".into()));
    }
    let scope = req.scope.unwrap_or_else(|| "jobs:execute".into());
    // R6-6: reject unknown scopes up-front rather than silently minting
    // a token that downstream control-plane endpoints would reject with
    // 403. The whitelist is small today (only jobs:execute) but this
    // guards against typos and future scopes wired unevenly.
    if !VALID_CONTROL_PLANE_SCOPES.contains(&scope.as_str()) {
        return Err(AdminError::BadRequest(format!(
            "unsupported scope: {scope:?}; valid scopes: {VALID_CONTROL_PLANE_SCOPES:?}"
        )));
    }
    let ttl = req
        .ttl_seconds
        .map(Duration::from_secs)
        .unwrap_or(Duration::ZERO);
    let (token, expires_at) = state.auth.mint_control_plane_jwt(
        &req.external_user_id,
        &scope,
        req.device_ids.clone(),
        ttl,
    )?;
    Ok(Json(ControlPlaneTokenResponse {
        token,
        external_user_id: req.external_user_id,
        scope,
        device_ids: req.device_ids,
        expires_at,
    }))
}

async fn delete_device(
    State(state): State<AppState>,
    Path(device_id): Path<String>,
) -> Result<StatusCode, AdminError> {
    let existing = state.devices.find_by_id(&device_id).await?;
    let existing_user = existing
        .as_ref()
        .and_then(|device| device.external_user_id.clone());
    let removed = state.devices.delete_device(&device_id).await?;
    if !removed {
        return Err(AdminError::NotFound);
    }
    // Best-effort: kick any live WS, emit the dashboard event, and
    // enqueue the outbound webhook. The row is already gone so none
    // of these failures are client-visible; we log at warn level
    // and return 204 either way.
    let _ = state.connections.kick_device(&device_id).await;
    if let Err(err) = state
        .events
        .emit_device_revoked(&device_id, existing_user.as_deref())
        .await
    {
        tracing::warn!(device_id = %device_id, error = %err, "failed to emit device.revoked event");
    }
    if let Err(err) = state
        .webhook
        .enqueue_revoked(&device_id, existing_user.as_deref())
        .await
    {
        tracing::warn!(
            device_id = %device_id,
            error = %err,
            "failed to enqueue device.revoked webhook",
        );
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListDevicesQuery {
    pub external_user_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminDeviceDto {
    pub device_id: String,
    pub external_user_id: Option<String>,
    pub hostname: String,
    pub os: String,
    pub capabilities: Vec<String>,
    pub version: Option<String>,
    pub online: bool,
    pub auth_method: String,
}

impl From<Device> for AdminDeviceDto {
    fn from(device: Device) -> Self {
        Self {
            device_id: device.id,
            external_user_id: device.external_user_id,
            hostname: device.hostname,
            os: device.os,
            capabilities: device.capabilities,
            version: device.version,
            online: device.online,
            auth_method: device.auth_method,
        }
    }
}

async fn list_devices(
    State(state): State<AppState>,
    query: Result<Query<ListDevicesQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Vec<AdminDeviceDto>>, AdminError> {
    let Query(query) =
        query.map_err(|_| AdminError::BadRequest("externalUserId is required".into()))?;
    if query.external_user_id.is_empty() {
        return Err(AdminError::BadRequest(
            "externalUserId must not be empty".into(),
        ));
    }
    let devices = state
        .devices
        .list_by_external_user(&query.external_user_id)
        .await?;
    Ok(Json(
        devices.into_iter().map(AdminDeviceDto::from).collect(),
    ))
}

#[derive(Debug)]
pub enum AdminError {
    Unauthorized,
    BadRequest(String),
    NotFound,
    Conflict(String),
    Internal(String),
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            AdminError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                "Authentication required".to_string(),
            ),
            AdminError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "VALIDATION_ERROR", msg),
            AdminError::NotFound => (
                StatusCode::NOT_FOUND,
                "DEVICE_NOT_FOUND",
                "Device not found".to_string(),
            ),
            AdminError::Conflict(msg) => {
                (StatusCode::CONFLICT, "DEVICE_OWNED_BY_DIFFERENT_USER", msg)
            }
            AdminError::Internal(msg) => {
                tracing::warn!(error = %msg, "admin API internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "INTERNAL_ERROR",
                    "Internal server error".to_string(),
                )
            }
        };
        (
            status,
            Json(ErrorEnvelope {
                error: ErrorBody { code, message },
            }),
        )
            .into_response()
    }
}

impl From<HubError> for AdminError {
    fn from(err: HubError) -> Self {
        match err {
            HubError::DeviceNotFound(_) => AdminError::NotFound,
            HubError::DeviceOwnedByDifferentUser {
                device_id,
                existing_external_user_id,
            } => AdminError::Conflict(format!(
                "Device {device_id} is owned by external user {existing_external_user_id}"
            )),
            HubError::Unauthorized | HubError::InvalidToken(_) | HubError::InvalidSignature => {
                AdminError::Unauthorized
            }
            other => AdminError::Internal(other.to_string()),
        }
    }
}

/// Re-export for wiring with `ahand_hub_core::auth::verify_*` in tests
/// and downstream tasks without leaking an extra dependency.
pub use auth::{
    verify_control_plane_jwt as verify_control_plane, verify_device_jwt as verify_device,
};
