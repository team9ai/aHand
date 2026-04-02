use thiserror::Error;

use crate::job::JobStatus;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HubError {
    #[error("device not found: {0}")]
    DeviceNotFound(String),
    #[error("device offline: {0}")]
    DeviceOffline(String),
    #[error("job not found: {0}")]
    JobNotFound(String),
    #[error("job not cancellable: {0}")]
    JobNotCancellable(String),
    #[error("illegal job transition: {current:?} -> {requested:?}")]
    IllegalJobTransition {
        current: JobStatus,
        requested: JobStatus,
    },
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("invalid token: {0}")]
    InvalidToken(String),
    #[error("invalid signature")]
    InvalidSignature,
    #[error("internal: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, HubError>;
