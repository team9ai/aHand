//! File operations module.
//!
//! Handles file/folder CRUD requested by the hub (via `FileRequest` envelopes)
//! and maps the results back to `FileResponse`. Policy is enforced per-path;
//! the actual filesystem work lives in the submodules.

pub mod binary_read;
pub mod fs_ops;
pub mod policy;
pub mod text_read;
pub mod write_ops;

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

    /// Pre-check a FileRequest's paths against policy *before* dispatching.
    ///
    /// Used by the request handler to decide whether the request should go
    /// through the approval flow (because the policy marks at least one of
    /// its paths as `dangerous_paths`) or proceed directly.
    ///
    /// - `Ok(true)`  — the request touches a dangerous path and must go
    ///   through approval regardless of session mode.
    /// - `Ok(false)` — no dangerous paths; normal session-mode flow applies.
    /// - `Err(FileError)` — one of the paths is flat-out denied by policy;
    ///   the caller should return this error immediately.
    pub fn check_request_approval(&self, req: &FileRequest) -> Result<bool, FileError> {
        let mut needs_approval = false;
        for (path, is_write, no_follow) in collect_request_paths(req) {
            let result = self.policy.check_path(&path, is_write, no_follow)?;
            if result.needs_approval {
                needs_approval = true;
            }
        }
        Ok(needs_approval)
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
            file_request::Operation::Stat(req) => {
                let checked =
                    self.policy
                        .check_path(&req.path, false, req.no_follow_symlink)?;
                let result = fs_ops::handle_stat(req, checked.resolved_path.as_path()).await?;
                Ok(file_response::Result::Stat(result))
            }
            file_request::Operation::List(req) => {
                let checked = self.policy.check_path(&req.path, false, false)?;
                let result = fs_ops::handle_list(req, checked.resolved_path.as_path()).await?;
                Ok(file_response::Result::List(result))
            }
            file_request::Operation::Glob(req) => {
                // Reject absolute and traversal glob patterns early. Without
                // this check, `/etc/**` or `../**` would let glob iterate
                // outside the base directory entirely; the per-match re-check
                // in handle_glob is a backstop but the pattern itself should
                // never have been accepted.
                if req.pattern.starts_with('/') {
                    return Err(file_error(
                        FileErrorCode::InvalidPath,
                        &req.pattern,
                        "absolute glob patterns are not allowed",
                    ));
                }
                if req.pattern.split(&['/', '\\'][..]).any(|seg| seg == "..") {
                    return Err(file_error(
                        FileErrorCode::InvalidPath,
                        &req.pattern,
                        "glob patterns must not contain .. components",
                    ));
                }

                let base_path_str = req.base_path.as_deref().unwrap_or("");
                let base: Option<std::path::PathBuf> = if base_path_str.is_empty() {
                    None
                } else {
                    let checked = self.policy.check_path(base_path_str, false, false)?;
                    Some(checked.resolved_path)
                };
                let result = fs_ops::handle_glob(req, base.as_deref(), &self.policy).await?;
                Ok(file_response::Result::Glob(result))
            }
            file_request::Operation::Mkdir(req) => {
                let checked = self.policy.check_path(&req.path, true, false)?;
                let result = fs_ops::handle_mkdir(req, checked.resolved_path.as_path()).await?;
                Ok(file_response::Result::Mkdir(result))
            }
            file_request::Operation::ReadText(req) => {
                let checked =
                    self.policy
                        .check_path(&req.path, false, req.no_follow_symlink)?;
                let result = text_read::handle_read_text(
                    req,
                    checked.resolved_path.as_path(),
                    self.policy.max_read_bytes(),
                )
                .await?;
                Ok(file_response::Result::ReadText(result))
            }
            file_request::Operation::ReadBinary(req) => {
                let checked =
                    self.policy
                        .check_path(&req.path, false, req.no_follow_symlink)?;
                let result = binary_read::handle_read_binary(
                    req,
                    checked.resolved_path.as_path(),
                    self.policy.max_read_bytes(),
                )
                .await?;
                Ok(file_response::Result::ReadBinary(result))
            }
            file_request::Operation::ReadImage(req) => {
                let checked =
                    self.policy
                        .check_path(&req.path, false, req.no_follow_symlink)?;
                let result = binary_read::handle_read_image(
                    req,
                    checked.resolved_path.as_path(),
                    self.policy.max_read_bytes(),
                )
                .await?;
                Ok(file_response::Result::ReadImage(result))
            }
            file_request::Operation::Write(req) => {
                let checked =
                    self.policy
                        .check_path(&req.path, true, req.no_follow_symlink)?;
                let result = write_ops::handle_write(
                    req,
                    checked.resolved_path.as_path(),
                    self.policy.max_write_bytes(),
                )
                .await?;
                Ok(file_response::Result::Write(result))
            }
            file_request::Operation::Edit(req) => {
                let checked =
                    self.policy
                        .check_path(&req.path, true, req.no_follow_symlink)?;
                let result = write_ops::handle_edit(
                    req,
                    checked.resolved_path.as_path(),
                    self.policy.max_write_bytes(),
                )
                .await?;
                Ok(file_response::Result::Edit(result))
            }
            file_request::Operation::Delete(req) => {
                let checked =
                    self.policy
                        .check_path(&req.path, true, req.no_follow_symlink)?;
                let result = fs_ops::handle_delete(req, checked.resolved_path.as_path()).await?;
                Ok(file_response::Result::Delete(result))
            }
            file_request::Operation::Chmod(req) => {
                let checked =
                    self.policy
                        .check_path(&req.path, true, req.no_follow_symlink)?;
                let result = fs_ops::handle_chmod(req, checked.resolved_path.as_path()).await?;
                Ok(file_response::Result::Chmod(result))
            }
            file_request::Operation::Copy(req) => {
                let source = self.policy.check_path(&req.source, false, false)?;
                let dest = self.policy.check_path(&req.destination, true, false)?;
                let result = fs_ops::handle_copy(
                    req,
                    source.resolved_path.as_path(),
                    dest.resolved_path.as_path(),
                )
                .await?;
                Ok(file_response::Result::Copy(result))
            }
            file_request::Operation::Move(req) => {
                let source = self.policy.check_path(&req.source, true, false)?;
                let dest = self.policy.check_path(&req.destination, true, false)?;
                let result = fs_ops::handle_move(
                    req,
                    source.resolved_path.as_path(),
                    dest.resolved_path.as_path(),
                )
                .await?;
                Ok(file_response::Result::MoveResult(result))
            }
            file_request::Operation::CreateSymlink(req) => {
                // Symlinks are created (not followed); the destination at
                // link_path must not be resolved through any pre-existing
                // symlink sitting there, so we use no_follow_symlink=true.
                let checked = self.policy.check_path(&req.link_path, true, true)?;
                let result =
                    fs_ops::handle_create_symlink(req, checked.resolved_path.as_path()).await?;
                Ok(file_response::Result::CreateSymlink(result))
            }
        }
    }
}

/// Walk a FileRequest and return every path it touches, alongside the
/// `is_write` and `no_follow_symlink` flags that dispatch would use.
/// Shared between `check_request_approval` (pre-flight) and the dispatch
/// arms in `FileManager::dispatch` — when adding a new operation, keep
/// both call sites in sync.
fn collect_request_paths(req: &FileRequest) -> Vec<(String, bool, bool)> {
    use file_request::Operation;
    let Some(op) = &req.operation else {
        return Vec::new();
    };
    match op {
        Operation::Stat(r) => vec![(r.path.clone(), false, r.no_follow_symlink)],
        Operation::List(r) => vec![(r.path.clone(), false, false)],
        Operation::Glob(r) => r
            .base_path
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|b| vec![(b.to_string(), false, false)])
            .unwrap_or_default(),
        Operation::Mkdir(r) => vec![(r.path.clone(), true, false)],
        Operation::ReadText(r) => vec![(r.path.clone(), false, r.no_follow_symlink)],
        Operation::ReadBinary(r) => vec![(r.path.clone(), false, r.no_follow_symlink)],
        Operation::ReadImage(r) => vec![(r.path.clone(), false, r.no_follow_symlink)],
        Operation::Write(r) => vec![(r.path.clone(), true, r.no_follow_symlink)],
        Operation::Edit(r) => vec![(r.path.clone(), true, r.no_follow_symlink)],
        Operation::Delete(r) => vec![(r.path.clone(), true, r.no_follow_symlink)],
        Operation::Chmod(r) => vec![(r.path.clone(), true, r.no_follow_symlink)],
        Operation::Copy(r) => vec![
            (r.source.clone(), false, false),
            (r.destination.clone(), true, false),
        ],
        Operation::Move(r) => vec![
            (r.source.clone(), true, false),
            (r.destination.clone(), true, false),
        ],
        Operation::CreateSymlink(r) => vec![(r.link_path.clone(), true, true)],
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
