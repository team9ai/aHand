//! File operations module.
//!
//! Handles file/folder CRUD requested by the hub (via `FileRequest` envelopes)
//! and maps the results back to `FileResponse`. Policy is enforced per-path;
//! the actual filesystem work lives in the submodules.

pub mod policy;

use ahand_protocol::{
    FileError, FileErrorCode, FileRequest, FileResponse, file_request, file_response,
};

use crate::config::FilePolicyConfig;
use policy::FilePolicyChecker;

/// Top-level file manager — dispatches `FileRequest` variants to submodule handlers.
#[derive(Debug, Clone)]
pub struct FileManager {
    policy: FilePolicyChecker,
    enabled: bool,
}

impl FileManager {
    pub fn new(config: &FilePolicyConfig) -> Self {
        Self {
            policy: FilePolicyChecker::new(config),
            enabled: config.enabled,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn policy(&self) -> &FilePolicyChecker {
        &self.policy
    }

    /// Dispatch a `FileRequest` to the appropriate submodule handler.
    pub async fn handle(&self, req: &FileRequest) -> FileResponse {
        let request_id = req.request_id.clone();

        if !self.enabled {
            return error_response(
                request_id,
                FileErrorCode::PolicyDenied,
                "",
                "file operations are disabled",
            );
        }

        match &req.operation {
            Some(op) => match self.dispatch(op).await {
                Ok(result) => FileResponse {
                    request_id,
                    result: Some(result),
                },
                Err(err) => FileResponse {
                    request_id,
                    result: Some(file_response::Result::Error(err)),
                },
            },
            None => error_response(
                request_id,
                FileErrorCode::Unspecified,
                "",
                "no operation specified",
            ),
        }
    }

    async fn dispatch(
        &self,
        op: &file_request::Operation,
    ) -> Result<file_response::Result, FileError> {
        match op {
            // Operations added in subsequent tasks return unspecified for now.
            file_request::Operation::Stat(req) => {
                let _result = self.policy.check_path(&req.path, false)?;
                Err(unimplemented_error(&req.path))
            }
            file_request::Operation::List(req) => {
                let _result = self.policy.check_path(&req.path, false)?;
                Err(unimplemented_error(&req.path))
            }
            file_request::Operation::Glob(req) => {
                let base = req.base_path.as_deref().unwrap_or("");
                let _result = self.policy.check_path(base, false)?;
                Err(unimplemented_error(base))
            }
            file_request::Operation::Mkdir(req) => {
                let _result = self.policy.check_path(&req.path, true)?;
                Err(unimplemented_error(&req.path))
            }
            file_request::Operation::ReadText(req) => {
                let _result = self.policy.check_path(&req.path, false)?;
                Err(unimplemented_error(&req.path))
            }
            file_request::Operation::ReadBinary(req) => {
                let _result = self.policy.check_path(&req.path, false)?;
                Err(unimplemented_error(&req.path))
            }
            file_request::Operation::ReadImage(req) => {
                let _result = self.policy.check_path(&req.path, false)?;
                Err(unimplemented_error(&req.path))
            }
            file_request::Operation::Write(req) => {
                let _result = self.policy.check_path(&req.path, true)?;
                Err(unimplemented_error(&req.path))
            }
            file_request::Operation::Edit(req) => {
                let _result = self.policy.check_path(&req.path, true)?;
                Err(unimplemented_error(&req.path))
            }
            file_request::Operation::Delete(req) => {
                let _result = self.policy.check_path(&req.path, true)?;
                Err(unimplemented_error(&req.path))
            }
            file_request::Operation::Chmod(req) => {
                let _result = self.policy.check_path(&req.path, true)?;
                Err(unimplemented_error(&req.path))
            }
            file_request::Operation::Copy(req) => {
                self.policy.check_path(&req.source, false)?;
                self.policy.check_path(&req.destination, true)?;
                Err(unimplemented_error(&req.destination))
            }
            file_request::Operation::Move(req) => {
                self.policy.check_path(&req.source, true)?;
                self.policy.check_path(&req.destination, true)?;
                Err(unimplemented_error(&req.destination))
            }
            file_request::Operation::CreateSymlink(req) => {
                let _result = self.policy.check_path(&req.link_path, true)?;
                Err(unimplemented_error(&req.link_path))
            }
        }
    }
}

/// Build a `FileResponse` carrying a `FileError`.
pub fn error_response(
    request_id: String,
    code: FileErrorCode,
    path: &str,
    message: &str,
) -> FileResponse {
    FileResponse {
        request_id,
        result: Some(file_response::Result::Error(FileError {
            code: code as i32,
            message: message.to_string(),
            path: path.to_string(),
        })),
    }
}

pub fn file_error(code: FileErrorCode, path: &str, message: impl Into<String>) -> FileError {
    FileError {
        code: code as i32,
        message: message.into(),
        path: path.to_string(),
    }
}

fn unimplemented_error(path: &str) -> FileError {
    file_error(
        FileErrorCode::Unspecified,
        path,
        "operation not yet implemented",
    )
}
