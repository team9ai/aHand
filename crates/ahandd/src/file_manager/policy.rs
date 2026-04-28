//! File policy checker — enforces path allowlist/denylist with canonicalization
//! and traversal detection.
//!
//! The checker is responsible for deciding whether a given path is allowed to
//! be accessed by a file operation. Canonicalization matters here: without it,
//! an attacker-controlled symlink inside an allowed directory can point at
//! `/etc/...` and the naive handler will operate on the outside target. This
//! module canonicalizes the path (resolving every symlink) before matching
//! against the allowlist.
//!
//! Paths that don't exist yet (destinations of Write/Mkdir/Copy/Move) are
//! handled by walking up the component tree to find the deepest existing
//! ancestor, canonicalizing that ancestor, and re-appending the suffix. The
//! `..` rejection in step 1 of `check_path` guarantees the suffix is already
//! free of traversal, so no escape is possible after re-joining.
//!
//! The checker also surfaces a `needs_approval` flag for paths listed in
//! `dangerous_paths`, so the dispatch layer can force STRICT approval for
//! those files regardless of the session mode.

use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};

use ahand_protocol::{FileError, FileErrorCode};

use crate::config::FilePolicyConfig;

/// The result of a path policy check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyResult {
    /// The resolved (canonicalized) path used for further operations.
    pub resolved_path: PathBuf,
    /// Whether this path is marked as "dangerous" and should trigger STRICT approval.
    pub needs_approval: bool,
}

/// File policy checker — stateless, clones configuration on construction.
#[derive(Debug, Clone)]
pub struct FilePolicyChecker {
    enabled: bool,
    allowlist: Vec<String>,
    denylist: Vec<String>,
    dangerous_paths: Vec<String>,
    max_read_bytes: u64,
    max_write_bytes: u64,
}

impl FilePolicyChecker {
    pub fn new(config: &FilePolicyConfig) -> Self {
        Self {
            enabled: config.enabled,
            allowlist: config.path_allowlist.clone(),
            denylist: config.path_denylist.clone(),
            dangerous_paths: config.dangerous_paths.clone(),
            max_read_bytes: config.max_read_bytes,
            max_write_bytes: config.max_write_bytes,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn max_read_bytes(&self) -> u64 {
        self.max_read_bytes
    }

    pub fn max_write_bytes(&self) -> u64 {
        self.max_write_bytes
    }

    /// Check whether a path is allowed by policy.
    ///
    /// Steps:
    /// 1. Reject paths containing `..` components (traversal detection).
    /// 2. Require absolute paths.
    /// 3. Canonicalize the path (or its deepest existing ancestor for
    ///    not-yet-created destinations). This step is what makes the check
    ///    robust against symlinks — the resolved target, not the input
    ///    string, is what gets matched.
    /// 4. Check denylist (takes precedence).
    /// 5. Check allowlist (empty = deny all).
    /// 6. Check dangerous paths (mark `needs_approval`).
    ///
    /// `no_follow_symlink` controls whether the final path component is
    /// resolved through a symlink. For operations that explicitly opt out of
    /// symlink following (e.g. `Stat { no_follow_symlink: true }` to inspect
    /// the symlink file itself), only the parent directory is canonicalized
    /// and the filename is re-appended. The parent is still fully resolved so
    /// it can't escape the allowlist via symlinks in the ancestor chain.
    pub fn check_path(
        &self,
        path: &str,
        _is_write: bool,
        no_follow_symlink: bool,
    ) -> Result<PolicyResult, FileError> {
        if !self.enabled {
            return Err(policy_error(
                FileErrorCode::PolicyDenied,
                path,
                "file operations are disabled in config",
            ));
        }

        if path.is_empty() {
            return Err(policy_error(
                FileErrorCode::InvalidPath,
                path,
                "path is empty",
            ));
        }

        // 1. Traversal detection — reject raw paths that contain `..`. This
        // runs BEFORE canonicalization so that `/home/user/../../etc/passwd`
        // gets rejected even if every component in it exists.
        let raw = Path::new(path);
        for comp in raw.components() {
            if matches!(comp, Component::ParentDir) {
                return Err(policy_error(
                    FileErrorCode::InvalidPath,
                    path,
                    "path traversal (..) is not allowed",
                ));
            }
        }

        // 2. Require absolute paths so patterns can match predictably.
        if !raw.is_absolute() {
            return Err(policy_error(
                FileErrorCode::InvalidPath,
                path,
                "only absolute paths are allowed",
            ));
        }

        // 3. Canonicalize the path (or its deepest existing ancestor) to
        // resolve symlinks before matching against patterns. When the caller
        // opts out of following the final symlink, we canonicalize only the
        // parent and re-append the filename.
        let resolved = if no_follow_symlink {
            canonicalize_no_follow(raw)
        } else {
            canonicalize_or_parent(raw)
        }
        .map_err(|e| {
            policy_error(
                FileErrorCode::InvalidPath,
                path,
                &format!("failed to resolve path: {e}"),
            )
        })?;
        let resolved_str = resolved.to_string_lossy().into_owned();

        // 4. Denylist — always takes precedence.
        for pattern in &self.denylist {
            if glob_match(pattern, &resolved_str) {
                return Err(policy_error(
                    FileErrorCode::PolicyDenied,
                    path,
                    "path is in the deny list",
                ));
            }
        }

        // 5. Allowlist — empty means deny all.
        if self.allowlist.is_empty() {
            return Err(policy_error(
                FileErrorCode::PolicyDenied,
                path,
                "allowlist is empty; no paths permitted",
            ));
        }
        let allowed = self
            .allowlist
            .iter()
            .any(|pattern| glob_match(pattern, &resolved_str));
        if !allowed {
            return Err(policy_error(
                FileErrorCode::PolicyDenied,
                path,
                "path is not in the allow list",
            ));
        }

        // 6. Dangerous paths — still allowed, but need approval.
        let needs_approval = self
            .dangerous_paths
            .iter()
            .any(|pattern| glob_match(pattern, &resolved_str));

        Ok(PolicyResult {
            resolved_path: resolved,
            needs_approval,
        })
    }
}

/// Canonicalize `path`, resolving symlinks. If `path` itself doesn't exist,
/// walk up the component tree to find the deepest existing ancestor,
/// canonicalize that ancestor, and re-append the remaining suffix.
///
/// Because `check_path` rejects any raw path containing `..`, the suffix is
/// guaranteed to be free of traversal components, so re-joining cannot escape
/// the canonicalized parent.
fn canonicalize_or_parent(path: &Path) -> io::Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(p) => return Ok(p),
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        Err(_) => {}
    }

    // Path doesn't exist yet. Walk up until we find an existing ancestor.
    let mut current = path.to_path_buf();
    let mut suffix: Vec<OsString> = Vec::new();
    loop {
        let Some(file_name) = current.file_name() else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no existing ancestor found",
            ));
        };
        suffix.push(file_name.to_os_string());
        if !current.pop() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no existing ancestor found",
            ));
        }
        match std::fs::canonicalize(&current) {
            Ok(canonical) => {
                let mut rebuilt = canonical;
                for part in suffix.iter().rev() {
                    rebuilt.push(part);
                }
                return Ok(rebuilt);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Canonicalize the parent directory of `path` and re-append the final
/// component without resolving it. Used when the caller opts out of following
/// the final symlink (e.g. `stat --no-dereference`, `chmod -h`).
///
/// The parent is still fully canonicalized, so a symlink in the ancestor
/// chain cannot be used to escape the allowlist.
fn canonicalize_no_follow(path: &Path) -> io::Result<PathBuf> {
    let file_name = path
        .file_name()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "path has no final component",
            )
        })?
        .to_os_string();
    let parent = path.parent().unwrap_or_else(|| Path::new("/"));
    let canonical_parent = canonicalize_or_parent(parent)?;
    Ok(canonical_parent.join(file_name))
}

/// Match a path against a glob pattern. Supports `*`, `**`, and `?`.
fn glob_match(pattern: &str, path: &str) -> bool {
    match glob::Pattern::new(pattern) {
        Ok(p) => p.matches(path),
        Err(_) => pattern == path,
    }
}

pub fn policy_error(code: FileErrorCode, path: &str, message: &str) -> FileError {
    FileError {
        code: code as i32,
        message: message.to_string(),
        path: path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Build a `FilePolicyConfig` whose allowlist covers the given temp dir.
    fn cfg_for_tempdir(
        root: &Path,
        extra_allow: &[String],
        denylist: &[String],
        dangerous: &[String],
    ) -> FilePolicyConfig {
        let root_str = root.to_string_lossy().into_owned();
        let mut allowlist = vec![format!("{}/**", root_str.trim_end_matches('/')), root_str];
        allowlist.extend(extra_allow.iter().cloned());
        FilePolicyConfig {
            enabled: true,
            path_allowlist: allowlist,
            path_denylist: denylist.to_vec(),
            max_read_bytes: 100_000_000,
            max_write_bytes: 100_000_000,
            dangerous_paths: dangerous.to_vec(),
        }
    }

    /// Canonicalize `tmp.path()` to its real path (macOS /var symlink handling).
    fn canon_root(tmp: &TempDir) -> PathBuf {
        tmp.path().canonicalize().unwrap()
    }

    #[test]
    fn path_within_allowlist() {
        let tmp = TempDir::new().unwrap();
        let root = canon_root(&tmp);
        let file = root.join("foo.txt");
        std::fs::write(&file, "x").unwrap();
        let checker = FilePolicyChecker::new(&cfg_for_tempdir(&root, &[], &[], &[]));
        let result = checker
            .check_path(&file.to_string_lossy(), false, false)
            .expect("path inside allowlist must pass");
        assert_eq!(result.resolved_path, file);
    }

    #[test]
    fn path_within_allowlist_not_yet_created() {
        // Destinations of mkdir/write don't exist yet — must still be allowed
        // when the deepest existing ancestor is inside the allowlist.
        let tmp = TempDir::new().unwrap();
        let root = canon_root(&tmp);
        let nonexistent = root.join("new_dir").join("new_file.txt");
        let checker = FilePolicyChecker::new(&cfg_for_tempdir(&root, &[], &[], &[]));
        let result = checker
            .check_path(&nonexistent.to_string_lossy(), true, false)
            .expect("not-yet-created path should pass via ancestor canonicalization");
        assert_eq!(result.resolved_path, nonexistent);
    }

    #[test]
    fn path_outside_allowlist() {
        let tmp = TempDir::new().unwrap();
        let root = canon_root(&tmp);
        let checker = FilePolicyChecker::new(&cfg_for_tempdir(&root, &[], &[], &[]));
        assert!(checker.check_path("/etc/hosts", false, false).is_err());
    }

    #[test]
    fn path_traversal_detected() {
        let tmp = TempDir::new().unwrap();
        let root = canon_root(&tmp);
        let checker = FilePolicyChecker::new(&cfg_for_tempdir(&root, &[], &[], &[]));
        let sneaky = format!("{}/../../../etc/passwd", root.display());
        let err = checker.check_path(&sneaky, false, false).unwrap_err();
        assert_eq!(err.code, FileErrorCode::InvalidPath as i32);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_pointing_outside_allowlist_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let root = canon_root(&tmp);
        // Create a symlink INSIDE the allowlist pointing at /etc/hosts which
        // is OUTSIDE. After canonicalization the resolved target should fall
        // outside the allowlist and be rejected.
        let link = root.join("escape_link");
        std::os::unix::fs::symlink("/etc/hosts", &link).unwrap();
        let checker = FilePolicyChecker::new(&cfg_for_tempdir(&root, &[], &[], &[]));
        let err = checker.check_path(&link.to_string_lossy(), false, false).unwrap_err();
        assert_eq!(err.code, FileErrorCode::PolicyDenied as i32);
    }

    #[test]
    fn denylist_overrides_allowlist() {
        let tmp = TempDir::new().unwrap();
        let root = canon_root(&tmp);
        let ssh_dir = root.join(".ssh");
        std::fs::create_dir(&ssh_dir).unwrap();
        let key = ssh_dir.join("id_rsa");
        std::fs::write(&key, "fake").unwrap();
        let denylist = vec![format!("{}/.ssh/**", root.display())];
        let checker = FilePolicyChecker::new(&cfg_for_tempdir(&root, &[], &denylist, &[]));
        let err = checker.check_path(&key.to_string_lossy(), false, false).unwrap_err();
        assert_eq!(err.code, FileErrorCode::PolicyDenied as i32);
    }

    #[test]
    fn dangerous_path_requires_approval() {
        let tmp = TempDir::new().unwrap();
        let root = canon_root(&tmp);
        let bashrc = root.join(".bashrc");
        std::fs::write(&bashrc, "x").unwrap();
        let dangerous = vec![format!("{}/.bashrc", root.display())];
        let checker = FilePolicyChecker::new(&cfg_for_tempdir(&root, &[], &[], &dangerous));
        let result = checker.check_path(&bashrc.to_string_lossy(), false, false).unwrap();
        assert!(result.needs_approval);
    }

    #[test]
    fn non_dangerous_path_does_not_require_approval() {
        let tmp = TempDir::new().unwrap();
        let root = canon_root(&tmp);
        let docs = root.join("docs.md");
        std::fs::write(&docs, "x").unwrap();
        let dangerous = vec![format!("{}/.bashrc", root.display())];
        let checker = FilePolicyChecker::new(&cfg_for_tempdir(&root, &[], &[], &dangerous));
        let result = checker.check_path(&docs.to_string_lossy(), false, false).unwrap();
        assert!(!result.needs_approval);
    }

    #[test]
    fn empty_allowlist_denies_all() {
        let tmp = TempDir::new().unwrap();
        let root = canon_root(&tmp);
        let file = root.join("foo.txt");
        std::fs::write(&file, "x").unwrap();
        // Explicit empty-allowlist config.
        let checker = FilePolicyChecker::new(&FilePolicyConfig {
            enabled: true,
            path_allowlist: Vec::new(),
            path_denylist: Vec::new(),
            max_read_bytes: 100_000_000,
            max_write_bytes: 100_000_000,
            dangerous_paths: Vec::new(),
        });
        let err = checker.check_path(&file.to_string_lossy(), false, false).unwrap_err();
        assert_eq!(err.code, FileErrorCode::PolicyDenied as i32);
    }

    #[test]
    fn relative_path_rejected() {
        let tmp = TempDir::new().unwrap();
        let root = canon_root(&tmp);
        let checker = FilePolicyChecker::new(&cfg_for_tempdir(&root, &[], &[], &[]));
        let err = checker.check_path("foo.txt", false, false).unwrap_err();
        assert_eq!(err.code, FileErrorCode::InvalidPath as i32);
    }

    #[test]
    fn disabled_policy_denies_all() {
        let tmp = TempDir::new().unwrap();
        let root = canon_root(&tmp);
        let mut config = cfg_for_tempdir(&root, &[], &[], &[]);
        config.enabled = false;
        let checker = FilePolicyChecker::new(&config);
        let file = root.join("foo.txt");
        std::fs::write(&file, "x").unwrap();
        let err = checker.check_path(&file.to_string_lossy(), false, false).unwrap_err();
        assert_eq!(err.code, FileErrorCode::PolicyDenied as i32);
    }

    #[test]
    fn empty_path_rejected() {
        let tmp = TempDir::new().unwrap();
        let root = canon_root(&tmp);
        let checker = FilePolicyChecker::new(&cfg_for_tempdir(&root, &[], &[], &[]));
        let err = checker.check_path("", false, false).unwrap_err();
        assert_eq!(err.code, FileErrorCode::InvalidPath as i32);
    }
}
