use axum::extract::rejection::JsonRejection;
use axum::extract::{Json, State};
use axum::http::StatusCode;
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::auth::AuthContextExt;
use crate::browser_service::{self, BrowserCommandInput, BrowserServiceError};
use crate::http::api_error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct BrowserCommandRequest {
    pub device_id: String,
    pub session_id: String,
    pub action: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_timeout_ms() -> u64 {
    30_000
}

#[derive(Serialize)]
pub struct BrowserCommandResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_mime: Option<String>,
}

pub async fn browser_command(
    auth: AuthContextExt,
    State(state): State<AppState>,
    body: Result<Json<BrowserCommandRequest>, JsonRejection>,
) -> ApiResult<Json<BrowserCommandResponse>> {
    auth.require_dashboard_access()?;
    let Json(body) = body.map_err(ApiError::from_json_rejection)?;

    let params_json = body
        .params
        .map(|p| serde_json::to_string(&p).unwrap_or_default())
        .unwrap_or_default();

    let response = browser_service::execute(
        &state,
        BrowserCommandInput {
            device_id: body.device_id,
            session_id: body.session_id,
            action: body.action,
            params_json,
            timeout_ms: body.timeout_ms,
            // Dashboard endpoint does not propagate a correlation id.
            // Task 9's worker handler will pass `Some(...)`.
            correlation_id: None,
        },
    )
    .await
    .map_err(map_service_error)?;

    let data = if response.result_json.is_empty() {
        None
    } else {
        serde_json::from_str(&response.result_json).ok()
    };

    let binary_data = if response.binary_data.is_empty() {
        None
    } else {
        Some(base64::engine::general_purpose::STANDARD.encode(&response.binary_data))
    };

    let binary_mime = if response.binary_mime.is_empty() {
        None
    } else {
        Some(response.binary_mime)
    };

    let error = if response.error.is_empty() {
        None
    } else {
        Some(response.error)
    };

    Ok(Json(BrowserCommandResponse {
        success: response.success,
        data,
        error,
        binary_data,
        binary_mime,
    }))
}

/// Translate a [`BrowserServiceError`] into the wire-format [`ApiError`]
/// the dashboard endpoint has historically returned.
///
/// Preserved verbatim from the pre-refactor `browser_command` handler:
///   - DEVICE_NOT_FOUND  -> 404 "Device {id} not found"
///   - DEVICE_OFFLINE    -> 404 "Device is not connected"
///   - DEVICE_OFFLINE    -> 404 "Failed to send to device: {err}"  (send-failure)
///   - NO_BROWSER_CAPABILITY -> 400 "Device does not support browser"
///   - TIMEOUT           -> 504 "Browser command timed out"
///   - INTERNAL_ERROR    -> 500 ("Browser request channel closed unexpectedly" | passthrough)
///
/// Task 9's `/api/control/browser` reuses this same mapper so both
/// endpoints surface the same error contract to clients.
pub(crate) fn map_service_error(err: BrowserServiceError) -> ApiError {
    match err {
        BrowserServiceError::DeviceNotFound { device_id } => ApiError::new(
            StatusCode::NOT_FOUND,
            "DEVICE_NOT_FOUND",
            format!("Device {device_id} not found"),
        ),
        BrowserServiceError::DeviceOffline { .. } => ApiError::new(
            StatusCode::NOT_FOUND,
            "DEVICE_OFFLINE",
            "Device is not connected",
        ),
        BrowserServiceError::SendFailed { reason, .. } => ApiError::new(
            StatusCode::NOT_FOUND,
            "DEVICE_OFFLINE",
            format!("Failed to send to device: {reason}"),
        ),
        BrowserServiceError::CapabilityMissing { .. } => ApiError::new(
            StatusCode::BAD_REQUEST,
            "NO_BROWSER_CAPABILITY",
            "Device does not support browser",
        ),
        BrowserServiceError::Timeout { .. } => ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "TIMEOUT",
            "Browser command timed out",
        ),
        BrowserServiceError::ChannelClosed => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "Browser request channel closed unexpectedly",
        ),
        BrowserServiceError::Internal(msg) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR", msg)
        }
    }
}
