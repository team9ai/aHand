use std::fmt::Display;

use ahand_hub_core::HubError;
use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
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

impl ApiError {
    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "Authentication required",
        )
    }

    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "FORBIDDEN", "Access denied")
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "VALIDATION_ERROR", message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR", message)
    }

    pub fn invalid_credentials() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "Invalid credentials",
        )
    }

    pub fn from_query_rejection(_value: QueryRejection) -> Self {
        Self::validation("Invalid query parameters")
    }

    pub fn gone(message: impl Into<String>) -> Self {
        Self::new(StatusCode::GONE, "JOB_FINISHED", message)
    }


    pub fn from_json_rejection(_value: JsonRejection) -> Self {
        Self::validation("Invalid JSON request body")
    }

    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn from_display(status: StatusCode, code: &'static str, message: impl Display) -> Self {
        Self::new(status, code, message.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

impl From<HubError> for ApiError {
    fn from(value: HubError) -> Self {
        match value {
            HubError::Unauthorized | HubError::InvalidToken(_) | HubError::InvalidSignature => {
                Self::unauthorized()
            }
            HubError::Forbidden => Self::forbidden(),
            HubError::DeviceNotFound(device_id) => Self::new(
                StatusCode::NOT_FOUND,
                "DEVICE_NOT_FOUND",
                format!("Device {device_id} was not found"),
            ),
            HubError::DeviceOffline(device_id) => Self::new(
                StatusCode::CONFLICT,
                "DEVICE_OFFLINE",
                format!("Device {device_id} is not currently connected"),
            ),
            HubError::DeviceAlreadyExists(device_id) => Self::new(
                StatusCode::CONFLICT,
                "VALIDATION_ERROR",
                format!("Device {device_id} already exists"),
            ),
            HubError::JobNotFound(job_id) => Self::new(
                StatusCode::NOT_FOUND,
                "JOB_NOT_FOUND",
                format!("Job {job_id} was not found"),
            ),
            HubError::JobNotCancellable(job_id) => Self::new(
                StatusCode::CONFLICT,
                "VALIDATION_ERROR",
                format!("Job {job_id} can no longer be cancelled"),
            ),
            HubError::IllegalJobTransition { current, requested } => Self::new(
                StatusCode::BAD_REQUEST,
                "VALIDATION_ERROR",
                format!("Illegal job transition: {current:?} -> {requested:?}"),
            ),
            HubError::InvalidPeerAck { ack, max } => Self::new(
                StatusCode::BAD_REQUEST,
                "VALIDATION_ERROR",
                format!("Invalid peer ack {ack}; max issued seq is {max}"),
            ),
            HubError::Internal(_) => Self::internal("Internal server error"),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(value: anyhow::Error) -> Self {
        match value.downcast::<HubError>() {
            Ok(err) => err.into(),
            Err(_) => Self::internal("Internal server error"),
        }
    }
}
