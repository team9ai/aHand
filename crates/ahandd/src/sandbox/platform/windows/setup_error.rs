//! Windows sandbox setup error types.
#![allow(dead_code)]

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::setup::sandbox_dir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SetupErrorCode {
    MarkerReadFailed,
    MarkerWriteFailed,
    MarkerDecodeFailed,
    UsersReadFailed,
    UsersWriteFailed,
    UsersDecodeFailed,
    SecretsReadFailed,
    SecretsWriteFailed,
    PasswordDecodeFailed,
    DpapiProtectFailed,
    DpapiUnprotectFailed,
    SetupErrorReportReadFailed,
    SetupErrorReportWriteFailed,
    SetupUnavailable,
    UsersGroupCreateFailed,
    UserCreateOrUpdateFailed,
    SidResolveFailed,
}

impl SetupErrorCode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::MarkerReadFailed => "marker_read_failed",
            Self::MarkerWriteFailed => "marker_write_failed",
            Self::MarkerDecodeFailed => "marker_decode_failed",
            Self::UsersReadFailed => "users_read_failed",
            Self::UsersWriteFailed => "users_write_failed",
            Self::UsersDecodeFailed => "users_decode_failed",
            Self::SecretsReadFailed => "secrets_read_failed",
            Self::SecretsWriteFailed => "secrets_write_failed",
            Self::PasswordDecodeFailed => "password_decode_failed",
            Self::DpapiProtectFailed => "dpapi_protect_failed",
            Self::DpapiUnprotectFailed => "dpapi_unprotect_failed",
            Self::SetupErrorReportReadFailed => "setup_error_report_read_failed",
            Self::SetupErrorReportWriteFailed => "setup_error_report_write_failed",
            Self::SetupUnavailable => "setup_unavailable",
            Self::UsersGroupCreateFailed => "users_group_create_failed",
            Self::UserCreateOrUpdateFailed => "user_create_or_update_failed",
            Self::SidResolveFailed => "sid_resolve_failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SetupErrorReport {
    pub(super) code: SetupErrorCode,
    pub(super) message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SetupFailure {
    pub(super) code: SetupErrorCode,
    pub(super) message: String,
}

impl SetupFailure {
    pub(super) fn new(code: SetupErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(super) fn unavailable(message: impl Into<String>) -> Self {
        Self::new(SetupErrorCode::SetupUnavailable, message)
    }

    pub(super) fn from_report(report: SetupErrorReport) -> Self {
        Self::new(report.code, report.message)
    }
}

impl std::fmt::Display for SetupFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for SetupFailure {}

pub(super) fn setup_error_path(state_root: &Path) -> PathBuf {
    sandbox_dir(state_root).join("setup_error.json")
}

pub(super) fn read_setup_error_report(
    state_root: &Path,
) -> Result<Option<SetupErrorReport>, SetupFailure> {
    let path = setup_error_path(state_root);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(SetupFailure::new(
                SetupErrorCode::SetupErrorReportReadFailed,
                format!("failed to read {}: {err}", path.display()),
            ));
        }
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::SetupErrorReportReadFailed,
            format!("failed to decode {}: {err}", path.display()),
        )
    })
}

pub(super) fn clear_setup_error_report(state_root: &Path) -> Result<(), SetupFailure> {
    let path = setup_error_path(state_root);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(SetupFailure::new(
            SetupErrorCode::SetupErrorReportWriteFailed,
            format!("failed to remove {}: {err}", path.display()),
        )),
    }
}
