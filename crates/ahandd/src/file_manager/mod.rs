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

use std::path::Path;

use ahand_protocol::{
    DeleteMode, FileError, FileErrorCode, FileRequest, FileResponse, file_request, file_response,
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
    /// through the approval flow. A request is escalated to approval when
    /// ANY of the following are true:
    ///
    /// - The policy marks at least one of the request's paths as dangerous
    ///   (`dangerous_paths` glob match).
    /// - For `FileGlob`, at least one matched path is dangerous (R4 —
    ///   without this, a glob over an allowed directory could silently
    ///   surface files in `dangerous_paths`).
    /// - The request is `FileDelete` with `mode = DELETE_MODE_PERMANENT`
    ///   and `recursive = true` (R9 + spec rule: recursive permanent delete
    ///   always forces approval regardless of session mode).
    ///
    /// Returns:
    /// - `Ok(true)` — the request must go through the approval flow.
    /// - `Ok(false)` — normal session-mode flow applies.
    /// - `Err(FileError)` — one of the paths is flat-out denied by policy;
    ///   the caller should return this error immediately.
    pub async fn check_request_approval(&self, req: &FileRequest) -> Result<bool, FileError> {
        // R9: recursive PERMANENT delete forces approval even if none of the
        // paths themselves are marked dangerous. The spec says this must
        // escalate regardless of session mode (design.md:635).
        if let Some(file_request::Operation::Delete(d)) = &req.operation {
            if d.mode == DeleteMode::Permanent as i32 && d.recursive {
                return Ok(true);
            }
        }

        // Walk the request's declared paths (R2 + existing behavior). The
        // collect_request_paths helper already includes both link_path AND
        // target for CreateSymlink (see R2).
        let mut needs_approval = false;
        for (path, is_write, no_follow) in collect_request_paths(req) {
            let result = self.policy.check_path(&path, is_write, no_follow)?;
            if result.needs_approval {
                needs_approval = true;
            }
        }
        if needs_approval {
            return Ok(true);
        }

        // R4: for Glob requests, also expand the pattern and check each
        // matched path. A glob rooted in an allowed directory can still
        // surface files in `dangerous_paths`, so we must pre-walk matches
        // here. Capped to avoid pathological patterns.
        if let Some(file_request::Operation::Glob(g)) = &req.operation {
            if glob_has_dangerous_match(&self.policy, g)? {
                return Ok(true);
            }
        }

        Ok(false)
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
                // R10: verify the just-created directory's canonical path
                // is still inside the allowlist (TOCTOU mitigation for
                // nonexistent-path symlink swaps).
                verify_post_create(&self.policy, checked.resolved_path.as_path()).await?;
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
                // R10: post-create verification for new files.
                verify_post_create(&self.policy, checked.resolved_path.as_path()).await?;
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
                // R10: verify the copy destination is still inside policy.
                verify_post_create(&self.policy, dest.resolved_path.as_path()).await?;
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
                // R10: verify the move destination is still inside policy.
                verify_post_create(&self.policy, dest.resolved_path.as_path()).await?;
                Ok(file_response::Result::MoveResult(result))
            }
            file_request::Operation::CreateSymlink(req) => {
                // Symlinks are created (not followed); the destination at
                // link_path must not be resolved through any pre-existing
                // symlink sitting there, so we use no_follow_symlink=true.
                let checked = self.policy.check_path(&req.link_path, true, true)?;
                // R2: also validate the target path against policy. An
                // absolute target is checked outright; a relative target is
                // resolved against the link's parent before checking. This
                // prevents creating an allowed symlink that points at
                // /etc/passwd and later using it as an allowlist bypass
                // surface through read operations that hit the canonical
                // target.
                let target_path = if Path::new(&req.target).is_absolute() {
                    std::path::PathBuf::from(&req.target)
                } else {
                    // Resolve relative target against the link's PARENT
                    // directory (that's what the OS does when resolving
                    // a relative symlink at read time).
                    let parent = checked
                        .resolved_path
                        .parent()
                        .unwrap_or_else(|| Path::new("/"));
                    parent.join(&req.target)
                };
                self.policy
                    .check_path(&target_path.to_string_lossy(), false, true)?;

                let result =
                    fs_ops::handle_create_symlink(req, checked.resolved_path.as_path()).await?;
                // R10: post-create verification — after the symlink exists,
                // re-check the link's own canonical path to catch any race.
                verify_post_create(&self.policy, checked.resolved_path.as_path()).await?;
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
        Operation::CreateSymlink(r) => {
            // R2: check BOTH the symlink's own location (link_path) and the
            // target string. The target is validated with no_follow=true +
            // is_write=false — it's a read-only reference stored inside the
            // symlink, not a path we operate on directly. If the target is
            // absolute and outside the allowlist, we refuse at creation
            // time so the symlink can't be used as an allowlist-bypass
            // vector later.
            let mut paths = vec![(r.link_path.clone(), true, true)];
            if !r.target.is_empty() && Path::new(&r.target).is_absolute() {
                paths.push((r.target.clone(), false, true));
            }
            paths
        }
    }
}

/// R4 helper: walk glob matches and return `true` as soon as any match is
/// flagged as dangerous by policy. Capped to avoid blocking on a
/// pathological pattern like `/**/*` that could enumerate millions of
/// files.
fn glob_has_dangerous_match(
    policy: &FilePolicyChecker,
    req: &ahand_protocol::FileGlob,
) -> Result<bool, FileError> {
    const GLOB_APPROVAL_SCAN_CAP: usize = 10_000;

    // Reject patterns that would have been rejected by dispatch anyway.
    if req.pattern.starts_with('/') {
        return Ok(false);
    }
    if req.pattern.split(&['/', '\\'][..]).any(|seg| seg == "..") {
        return Ok(false);
    }

    // Resolve the pattern against the (policy-canonicalized) base_path.
    let base = match req.base_path.as_deref() {
        Some(b) if !b.is_empty() => {
            // The caller's check_request_approval already ran
            // policy.check_path on the base_path, so we can trust it.
            match policy.check_path(b, false, false) {
                Ok(r) => Some(r.resolved_path),
                Err(_) => return Ok(false),
            }
        }
        _ => None,
    };
    let full_pattern = match base {
        Some(b) => b.join(&req.pattern).to_string_lossy().into_owned(),
        None => req.pattern.clone(),
    };

    let glob_iter = match glob::glob(&full_pattern) {
        Ok(g) => g,
        Err(_) => return Ok(false),
    };

    for entry in glob_iter.take(GLOB_APPROVAL_SCAN_CAP) {
        let Ok(path) = entry else {
            continue;
        };
        let path_str = path.to_string_lossy();
        match policy.check_path(&path_str, false, false) {
            Ok(result) if result.needs_approval => return Ok(true),
            _ => {}
        }
    }
    Ok(false)
}

/// R10: post-create verification to mitigate the nonexistent-path symlink
/// TOCTOU race. The canonicalize_or_parent helper in policy rebuilds a
/// non-existing path from its deepest existing ancestor, so an attacker
/// who swaps a component for a symlink between the policy check and the
/// operation can redirect the write/mkdir target. Re-canonicalizing AFTER
/// the operation catches most such swaps; anything escaping the allowlist
/// is cleaned up (best-effort) and reported as PolicyDenied.
///
/// Full TOCTOU protection requires fd-based syscalls (openat2 +
/// RESOLVE_NO_SYMLINKS on Linux, O_NOFOLLOW elsewhere) — that refactor is
/// deferred to a follow-up PR.
async fn verify_post_create(policy: &FilePolicyChecker, resolved: &Path) -> Result<(), FileError> {
    let path_str = resolved.to_string_lossy();
    match policy.check_path(&path_str, false, false) {
        Ok(_) => Ok(()),
        Err(err) => {
            // Best-effort cleanup: try file removal first, then directory.
            // If the created resource can't be removed, leave it in place
            // and still return the error so the caller knows the op was
            // rejected.
            if let Ok(metadata) = tokio::fs::symlink_metadata(resolved).await {
                if metadata.is_dir() {
                    let _ = tokio::fs::remove_dir(resolved).await;
                } else {
                    let _ = tokio::fs::remove_file(resolved).await;
                }
            }
            Err(err)
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

/// When `no_follow_symlink` is set, refuse to operate on the final component
/// if it is a symlink. This is the defense layer after policy has already
/// done its parent-canonicalization — we want to make sure the handler
/// itself never calls a follow-by-default syscall like `tokio::fs::write`
/// on a symlink target.
pub(super) async fn reject_if_final_component_is_symlink(
    resolved: &std::path::Path,
    req_path: &str,
    no_follow_symlink: bool,
) -> Result<(), FileError> {
    if !no_follow_symlink {
        return Ok(());
    }
    // symlink_metadata never follows. If the file doesn't exist at all, we
    // let the downstream handler produce its own NotFound error.
    if let Ok(metadata) = tokio::fs::symlink_metadata(resolved).await {
        if metadata.file_type().is_symlink() {
            return Err(file_error(
                FileErrorCode::InvalidPath,
                req_path,
                "no_follow_symlink is set but the final component is a symlink",
            ));
        }
    }
    Ok(())
}

fn unimplemented_error(path: &str) -> FileError {
    file_error(
        FileErrorCode::Unspecified,
        path,
        "operation not yet implemented",
    )
}
