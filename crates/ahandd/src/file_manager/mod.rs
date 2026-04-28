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

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use ahand_protocol::{
    DeleteMode, FileError, FileErrorCode, FileRequest, FileResponse, file_request, file_response,
};

use crate::config::FilePolicyConfig;
use policy::FilePolicyChecker;

/// Cap on how many glob matches we walk during the pre-flight approval scan.
/// Past this point we fail closed (`GlobScanOutcome::CapHit`) — see C3.
///
/// Stored as an atomic so integration tests (which can't use `cfg(test)` on
/// the library crate) can lower it via [`set_glob_approval_scan_cap`] without
/// having to materialize 10k+ files. Loaded once per scan; the atomic cost
/// is negligible next to the filesystem walk.
static GLOB_APPROVAL_SCAN_CAP: AtomicUsize = AtomicUsize::new(10_000);

/// Override the glob approval scan cap. Returns the previous value so the
/// caller can restore it.
///
/// **Test-only.** This function exists so integration tests can lower the
/// cap to a small number (e.g. 8) and exercise the `CapHit` branch
/// without materializing 10k+ files on disk. It has no production use
/// case — calling it from production code would degrade the C3
/// fail-closed safety guarantee by either raising the cap (more files
/// scanned, more time to abort) or lowering it (every glob escalates,
/// surfacing as approval-storm UX bugs).
///
/// We considered gating this behind `cfg(any(test, feature = "test-utils"))`
/// but Cargo's integration-test build doesn't see the lib's `cfg(test)`
/// and `required-features` on `[[test]]` targets makes plain `cargo test`
/// silently skip those targets, which breaks CI ergonomics. The doc-hidden
/// + naming-by-shame approach is what we have today; the function is
/// `#[doc(hidden)]` so it doesn't surface in rustdoc and the body of the
/// docstring tells anyone reaching for it to stop.
#[doc(hidden)]
pub fn set_glob_approval_scan_cap(cap: usize) -> usize {
    GLOB_APPROVAL_SCAN_CAP.swap(cap, Ordering::Relaxed)
}

/// Why `check_request_approval` decided a request must go through approval.
/// The `reason` string is what surfaces in the approval prompt UI; `path`
/// is the specific path that triggered the escalation, when applicable.
#[derive(Debug, Clone)]
pub struct ApprovalEscalation {
    pub kind: EscalationKind,
    pub reason: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationKind {
    RecursivePermanentDelete,
    DangerousPath,
    DangerousGlobMatch,
    /// Glob's safety cap was hit before we could check every match — we
    /// can't prove the unscanned tail is safe, so we force approval.
    GlobScanCapHit,
}

/// Outcome of pre-walking a glob pattern for `dangerous_paths` matches.
#[derive(Debug)]
enum GlobScanOutcome {
    /// At least one matched path is in `dangerous_paths`. Carries the path
    /// so the approval prompt can name the specific match.
    FoundDangerous(String),
    /// We scanned `GLOB_APPROVAL_SCAN_CAP` matches without finding a
    /// dangerous one, but more matches remained. C3: fail closed — the
    /// caller treats this the same as "found one" (force approval).
    CapHit,
    /// No dangerous matches in the entire glob expansion (under the cap).
    Clean,
}

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

    /// Return the list of paths a FileRequest touches, for display in
    /// approval prompts (R8). Mirrors `collect_request_paths` but drops
    /// the is_write / no_follow_symlink flags so the caller can use the
    /// result directly as `ApprovalRequest.args`.
    pub fn request_paths(&self, req: &FileRequest) -> Vec<String> {
        collect_request_paths(req)
            .into_iter()
            .map(|(p, _, _)| p)
            .collect()
    }

    /// Pre-check a FileRequest's paths against policy *before* dispatching.
    ///
    /// Used by the request handler to decide whether the request should go
    /// through the approval flow. Returns:
    /// - `Ok(None)` — no escalation needed; normal session-mode flow applies.
    /// - `Ok(Some(reason))` — request must go through approval; the reason
    ///   carries enough context for the approval prompt (which path
    ///   triggered it and why).
    /// - `Err(FileError)` — a path is flat-out denied by policy; the caller
    ///   should return this error immediately.
    ///
    /// Escalation conditions (any one of these triggers approval):
    /// - **Recursive PERMANENT delete** — spec rule design.md:635.
    /// - **Dangerous-path match** — any path the request touches is in
    ///   `dangerous_paths`. This now correctly resolves relative symlink
    ///   targets too.
    /// - **Glob over a tree containing dangerous paths** — `glob_has_*`
    ///   walks matches with a safety cap and FAILS CLOSED at the cap (i.e.
    ///   we conservatively force approval rather than letting an unscanned
    ///   tail bypass it).
    pub async fn check_request_approval(
        &self,
        req: &FileRequest,
    ) -> Result<Option<ApprovalEscalation>, FileError> {
        // Recursive PERMANENT delete forces approval even if none of the
        // paths themselves are marked dangerous (design.md:635).
        if let Some(file_request::Operation::Delete(d)) = &req.operation {
            if d.mode == DeleteMode::Permanent as i32 && d.recursive {
                return Ok(Some(ApprovalEscalation {
                    kind: EscalationKind::RecursivePermanentDelete,
                    reason: "recursive permanent delete requires explicit approval"
                        .to_string(),
                    path: Some(d.path.clone()),
                }));
            }
        }

        // Walk the request's declared paths. collect_request_paths now
        // resolves relative CreateSymlink targets against the link's
        // parent (C2), so a relative target hitting `dangerous_paths` is
        // correctly escalated here.
        for (path, is_write, no_follow) in collect_request_paths(req) {
            let result = self.policy.check_path(&path, is_write, no_follow)?;
            if result.needs_approval {
                return Ok(Some(ApprovalEscalation {
                    kind: EscalationKind::DangerousPath,
                    reason: format!("path '{}' is listed in dangerous_paths", path),
                    path: Some(path),
                }));
            }
        }

        // Glob expansion: walk matches and either find a dangerous one or
        // hit the safety cap. C3: at the cap we FAIL CLOSED — conservatively
        // require approval rather than treating "didn't find one in the
        // first 10k matches" as "safe".
        if let Some(file_request::Operation::Glob(g)) = &req.operation {
            match glob_has_dangerous_match(&self.policy, g)? {
                GlobScanOutcome::FoundDangerous(p) => {
                    return Ok(Some(ApprovalEscalation {
                        kind: EscalationKind::DangerousGlobMatch,
                        reason: format!(
                            "glob pattern matches a path '{}' listed in dangerous_paths",
                            p
                        ),
                        path: Some(p),
                    }));
                }
                GlobScanOutcome::CapHit => {
                    return Ok(Some(ApprovalEscalation {
                        kind: EscalationKind::GlobScanCapHit,
                        reason: format!(
                            "glob pattern matches more than {} paths; approval required \
                             because not all matches could be checked against dangerous_paths",
                            GLOB_APPROVAL_SCAN_CAP.load(Ordering::Relaxed)
                        ),
                        path: None,
                    }));
                }
                GlobScanOutcome::Clean => {}
            }
        }

        Ok(None)
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
                    // a relative symlink at read time). Lexically
                    // normalize so the resulting path doesn't carry raw
                    // `..` components — `policy.check_path` would
                    // otherwise reject the post-approval dispatch with
                    // `InvalidPath`, even though the approval prompt
                    // (which uses the same normalization in
                    // `collect_request_paths`) showed the user a clean
                    // canonical path. Without this, every approved
                    // relative-target symlink that escapes its own
                    // parent would fail at execution time.
                    let parent = checked
                        .resolved_path
                        .parent()
                        .unwrap_or_else(|| Path::new("/"));
                    lexically_normalize(&parent.join(&req.target))
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

/// Lexical (purely textual) path normalization — collapses `.` and `..`
/// components without touching the filesystem and without following
/// symlinks. Used to resolve relative symlink targets so the policy
/// checker sees a clean, canonical-shape path it can match against
/// `dangerous_paths` entries.
///
/// We deliberately do NOT use `canonicalize` here: we want to evaluate
/// the symlink-target string as a literal reference, not as something
/// to follow on disk (which would also fail when the target doesn't
/// exist yet, which is the common case).
fn lexically_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                // pop() returns false when out is empty or the only
                // component is a root/prefix; in either case we just
                // can't go further up, which is fine — we leave `out`
                // as-is and the resulting path stays inside whatever
                // root we started with.
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
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
            // Check BOTH the symlink's own location (link_path) and the
            // target string. The target is validated with no_follow=true +
            // is_write=false — it's a read-only reference stored inside the
            // symlink, not a path we operate on directly.
            //
            // C2: relative targets are resolved against the link's parent
            // directory and lexically normalized BEFORE the policy check.
            // Without this, a `target = "../secret.txt"` attached to an
            // allowed link_path in `<root>/sub/` would either bypass
            // `dangerous_paths` entirely (if we skipped relative targets)
            // or get rejected by the unrelated path-traversal guard (if
            // we passed `<root>/sub/../secret.txt` raw to the checker).
            // Lexical normalization gives us `<root>/secret.txt`, which
            // dangerous_paths can match correctly.
            let mut paths = vec![(r.link_path.clone(), true, true)];
            if !r.target.is_empty() {
                let target = Path::new(&r.target);
                if target.is_absolute() {
                    paths.push((r.target.clone(), false, true));
                } else if let Some(parent) = Path::new(&r.link_path).parent() {
                    let resolved = lexically_normalize(&parent.join(target));
                    paths.push((resolved.to_string_lossy().into_owned(), false, true));
                }
            }
            paths
        }
    }
}

/// Walk glob matches and either find a dangerous one or hit the safety cap.
///
/// C3: returns `CapHit` when the cap is reached — the caller treats this
/// the same as `FoundDangerous` (force approval). Previously we returned
/// `false` past the cap, which fails OPEN: a pattern matching 10_001 paths
/// where the dangerous one happened to be the 10_001st would silently slip
/// through.
///
/// We still cap; the only change is what "we ran out of budget" means.
fn glob_has_dangerous_match(
    policy: &FilePolicyChecker,
    req: &ahand_protocol::FileGlob,
) -> Result<GlobScanOutcome, FileError> {
    // Reject patterns that would have been rejected by dispatch anyway.
    // These are syntactic violations, not "dangerous-path" hits — there
    // are zero legitimate matches to scan, so the answer is Clean.
    if req.pattern.starts_with('/') {
        return Ok(GlobScanOutcome::Clean);
    }
    if req.pattern.split(&['/', '\\'][..]).any(|seg| seg == "..") {
        return Ok(GlobScanOutcome::Clean);
    }

    // Resolve the pattern against the (policy-canonicalized) base_path.
    let base = match req.base_path.as_deref() {
        Some(b) if !b.is_empty() => {
            // The caller's check_request_approval already ran
            // policy.check_path on the base_path, so we can trust it.
            match policy.check_path(b, false, false) {
                Ok(r) => Some(r.resolved_path),
                Err(_) => return Ok(GlobScanOutcome::Clean),
            }
        }
        _ => None,
    };
    let full_pattern = match base {
        Some(b) => b.join(&req.pattern).to_string_lossy().into_owned(),
        None => req.pattern.clone(),
    };

    let mut glob_iter = match glob::glob(&full_pattern) {
        Ok(g) => g,
        Err(_) => return Ok(GlobScanOutcome::Clean),
    };

    let cap = GLOB_APPROVAL_SCAN_CAP.load(Ordering::Relaxed);
    let mut scanned = 0usize;
    while scanned < cap {
        let Some(entry) = glob_iter.next() else {
            return Ok(GlobScanOutcome::Clean);
        };
        scanned += 1;
        let Ok(path) = entry else {
            continue;
        };
        let path_str = path.to_string_lossy();
        if let Ok(result) = policy.check_path(&path_str, false, false)
            && result.needs_approval
        {
            return Ok(GlobScanOutcome::FoundDangerous(path_str.into_owned()));
        }
    }

    // We hit the cap. If at least one more match exists, we can't prove
    // safety — fail closed. If the iterator is exhausted exactly at the
    // cap, we did finish scanning; treat as Clean.
    if glob_iter.next().is_some() {
        Ok(GlobScanOutcome::CapHit)
    } else {
        Ok(GlobScanOutcome::Clean)
    }
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
            //
            // KNOWN LIMITATIONS — see also the round-4 follow-up issue:
            //
            // 1. **Move can lose data.** Caller arms: `handle_move`
            //    `rename`s the source to `resolved` *before* this
            //    function runs. If `policy.check_path` then rejects
            //    `resolved` (TOCTOU swap on a parent component slipped
            //    a symlink past the pre-check), the cleanup deletes
            //    `resolved` — and the original source is already gone.
            //    The caller observes `PolicyDenied` with no recovery
            //    path. Full fix needs `openat2(RESOLVE_NO_SYMLINKS)`
            //    or equivalent so the rename target can't be diverted.
            //
            // 2. **Copy of a directory tree leaves a partial copy.**
            //    `remove_dir` (below) does NOT recurse, so a
            //    recursive copy that hits a post-op canonical
            //    rejection leaves the partial tree in the rejected
            //    location. The source is intact, so this is a leak
            //    rather than data loss; using `remove_dir_all` would
            //    fully clean up but also runs against the same
            //    TOCTOU surface (an attacker who can swap a parent
            //    can reroute the recursive remove).
            //
            // Both issues are already implicit in the "best-effort"
            // wording, but we name them here so future readers don't
            // have to re-derive the failure modes.
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
