use ahand_hub_core::auth::Role;
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};

use crate::auth::AuthContextExt;
use crate::http::api_error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub token: String,
}

#[derive(Debug, Serialize)]
pub struct VerifyResponse {
    pub subject: String,
    pub role: Role,
    pub iss: String,
}

pub async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> ApiResult<Json<LoginResponse>> {
    if body.password != state.dashboard_shared_password.as_str() {
        state
            .append_audit_entry(
                "auth.login_failed",
                "auth",
                "dashboard",
                "dashboard",
                serde_json::json!({ "reason": "invalid_credentials" }),
            )
            .await;
        return Err(ApiError::invalid_credentials());
    }

    let token = state
        .auth
        .issue_dashboard_jwt("dashboard")
        .map_err(|_| ApiError::internal("Failed to issue dashboard token"))?;

    state
        .append_audit_entry(
            "auth.login_success",
            "auth",
            "dashboard",
            "dashboard",
            serde_json::json!({}),
        )
        .await;

    Ok(Json(LoginResponse { token }))
}

pub async fn verify(auth: AuthContextExt) -> ApiResult<Json<VerifyResponse>> {
    auth.require_dashboard_access()?;
    Ok(Json(VerifyResponse {
        subject: auth.0.subject,
        role: auth.0.role,
        iss: auth.0.iss,
    }))
}
