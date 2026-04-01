use ahand_hub_core::auth::Role;
use axum::{
    Json,
    extract::State,
    http::StatusCode,
};
use serde::{Deserialize, Serialize};

use crate::auth::AuthContextExt;
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

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: &'static str,
}

pub async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, (StatusCode, Json<ErrorResponse>)> {
    if body.password != state.dashboard_shared_password.as_str() {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "invalid_credentials",
            }),
        ));
    }

    let token = state.auth.issue_dashboard_jwt("dashboard").map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "token_issue_failed",
            }),
        )
    })?;

    Ok(Json(LoginResponse { token }))
}

pub async fn verify(auth: AuthContextExt) -> Result<Json<VerifyResponse>, StatusCode> {
    auth.require_dashboard_access()?;
    Ok(Json(VerifyResponse {
        subject: auth.0.subject,
        role: auth.0.role,
        iss: auth.0.iss,
    }))
}
