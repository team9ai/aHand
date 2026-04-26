use ahand_hub_core::traits::DeviceStore;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Json, State};
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::auth::AuthContextExt;
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

    let device = state
        .devices
        .get(&body.device_id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::new(
                axum::http::StatusCode::NOT_FOUND,
                "DEVICE_NOT_FOUND",
                format!("Device {} not found", body.device_id),
            )
        })?;

    if !device.online {
        return Err(ApiError::new(
            axum::http::StatusCode::NOT_FOUND,
            "DEVICE_OFFLINE",
            "Device is not connected",
        ));
    }

    if !device.capabilities.iter().any(|c| c == "browser") {
        return Err(ApiError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "NO_BROWSER_CAPABILITY",
            "Device does not support browser",
        ));
    }

    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.browser_pending.insert(request_id.clone(), tx);

    let params_json = body
        .params
        .map(|p| serde_json::to_string(&p).unwrap_or_default())
        .unwrap_or_default();

    let envelope = ahand_protocol::Envelope {
        device_id: body.device_id.clone(),
        msg_id: format!("browser-{request_id}"),
        ts_ms: now_ms(),
        payload: Some(ahand_protocol::envelope::Payload::BrowserRequest(
            ahand_protocol::BrowserRequest {
                request_id: request_id.clone(),
                session_id: body.session_id,
                action: body.action,
                params_json,
                timeout_ms: body.timeout_ms,
            },
        )),
        ..Default::default()
    };

    if let Err(err) = state.connections.send(&body.device_id, envelope).await {
        state.browser_pending.remove(&request_id);
        return Err(ApiError::new(
            axum::http::StatusCode::NOT_FOUND,
            "DEVICE_OFFLINE",
            format!("Failed to send to device: {err}"),
        ));
    }

    let timeout = std::time::Duration::from_millis(body.timeout_ms.max(1000));
    let response = match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(_)) => {
            state.browser_pending.remove(&request_id);
            return Err(ApiError::new(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "Browser request channel closed unexpectedly",
            ));
        }
        Err(_) => {
            state.browser_pending.remove(&request_id);
            return Err(ApiError::new(
                axum::http::StatusCode::GATEWAY_TIMEOUT,
                "TIMEOUT",
                "Browser command timed out",
            ));
        }
    };

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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
