//! File policy checker — enforces path allowlist/denylist with traversal detection.
//!
//! This module is responsible for deciding whether a given path is allowed to be
//! accessed by a file operation. It integrates with the daemon's broader session
//! mode (STRICT / TRUST / AUTO_ACCEPT) via the `needs_approval` flag on
//! `PolicyResult`.

use std::path::{Component, Path, PathBuf};

use ahand_protocol::{FileError, FileErrorCode};

use crate::config::FilePolicyConfig;

/// The result of a path policy check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyResult {
    /// The resolved (normalized) path used for further operations.
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
    /// 2. Normalize the path (absolute form, no `.`/`..`).
    /// 3. Check denylist (takes precedence).
    /// 4. Check allowlist (empty = deny all).
    /// 5. Check dangerous paths (mark `needs_approval`).
    pub fn check_path(&self, path: &str, _is_write: bool) -> Result<PolicyResult, FileError> {
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

        // 1. Traversal detection — reject raw paths that contain `..`.
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

        // 2. Normalize: require absolute paths so patterns can match predictably.
        if !raw.is_absolute() {
            return Err(policy_error(
                FileErrorCode::InvalidPath,
                path,
                "only absolute paths are allowed",
            ));
        }

        let resolved = normalize_path(raw);
        let resolved_str = resolved.to_string_lossy().into_owned();

        // 3. Denylist — always takes precedence.
        for pattern in &self.denylist {
            if glob_match(pattern, &resolved_str) {
                return Err(policy_error(
                    FileErrorCode::PolicyDenied,
                    path,
                    "path is in the deny list",
                ));
            }
        }

        // 4. Allowlist — empty means deny all.
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

        // 5. Dangerous paths — still allowed, but need approval.
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

/// Normalize a path by collapsing `.` and `..` components.
///
/// This does NOT resolve symlinks (that's the caller's responsibility when needed).
/// It ensures that a rejected traversal like `/home/user/../../etc` is caught even
/// when the inputs are otherwise well-formed.
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
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

    fn cfg(allowlist: &[&str], denylist: &[&str], dangerous: &[&str]) -> FilePolicyConfig {
        FilePolicyConfig {
            enabled: true,
            path_allowlist: allowlist.iter().map(|s| s.to_string()).collect(),
            path_denylist: denylist.iter().map(|s| s.to_string()).collect(),
            max_read_bytes: 100_000_000,
            max_write_bytes: 100_000_000,
            dangerous_paths: dangerous.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn path_within_allowlist() {
        let checker = FilePolicyChecker::new(&cfg(&["/home/user/**"], &[], &[]));
        assert!(checker.check_path("/home/user/foo.txt", false).is_ok());
    }

    #[test]
    fn path_outside_allowlist() {
        let checker = FilePolicyChecker::new(&cfg(&["/home/user/**"], &[], &[]));
        assert!(checker.check_path("/etc/passwd", false).is_err());
    }

    #[test]
    fn path_traversal_detected() {
        let checker = FilePolicyChecker::new(&cfg(&["/home/user/**"], &[], &[]));
        let err = checker
            .check_path("/home/user/../../etc/passwd", false)
            .unwrap_err();
        assert_eq!(err.code, FileErrorCode::InvalidPath as i32);
    }

    #[test]
    fn denylist_overrides_allowlist() {
        let checker =
            FilePolicyChecker::new(&cfg(&["/home/user/**"], &["/home/user/.ssh/**"], &[]));
        assert!(checker.check_path("/home/user/.ssh/id_rsa", false).is_err());
    }

    #[test]
    fn dangerous_path_requires_approval() {
        let checker =
            FilePolicyChecker::new(&cfg(&["/home/user/**"], &[], &["/home/user/.bashrc"]));
        let result = checker.check_path("/home/user/.bashrc", false).unwrap();
        assert!(result.needs_approval);
    }

    #[test]
    fn non_dangerous_path_does_not_require_approval() {
        let checker =
            FilePolicyChecker::new(&cfg(&["/home/user/**"], &[], &["/home/user/.bashrc"]));
        let result = checker.check_path("/home/user/docs.md", false).unwrap();
        assert!(!result.needs_approval);
    }

    #[test]
    fn empty_allowlist_denies_all() {
        let checker = FilePolicyChecker::new(&cfg(&[], &[], &[]));
        assert!(checker.check_path("/home/user/foo.txt", false).is_err());
    }

    #[test]
    fn relative_path_rejected() {
        let checker = FilePolicyChecker::new(&cfg(&["/home/user/**"], &[], &[]));
        let err = checker.check_path("foo.txt", false).unwrap_err();
        assert_eq!(err.code, FileErrorCode::InvalidPath as i32);
    }

    #[test]
    fn disabled_policy_denies_all() {
        let mut config = cfg(&["/home/user/**"], &[], &[]);
        config.enabled = false;
        let checker = FilePolicyChecker::new(&config);
        let err = checker.check_path("/home/user/foo.txt", false).unwrap_err();
        assert_eq!(err.code, FileErrorCode::PolicyDenied as i32);
    }

    #[test]
    fn empty_path_rejected() {
        let checker = FilePolicyChecker::new(&cfg(&["/home/user/**"], &[], &[]));
        let err = checker.check_path("", false).unwrap_err();
        assert_eq!(err.code, FileErrorCode::InvalidPath as i32);
    }
}
