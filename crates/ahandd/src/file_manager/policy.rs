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

#[allow(dead_code)] // is_enabled() is a public accessor used by future policy auditing
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
        // Strip the Windows verbatim prefix (`\\?\`) before returning so
        // the result is always comparable with plain config patterns.
        Ok(p) => return Ok(ahand_platform::paths::simplify(&p)),
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
                // Strip the Windows verbatim prefix before rebuilding so
                // the re-joined path is pattern-matchable without a prefix.
                let mut rebuilt = ahand_platform::paths::simplify(&canonical);
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
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no final component"))?
        .to_os_string();
    let parent = path.parent().unwrap_or_else(|| Path::new("/"));
    let canonical_parent = canonicalize_or_parent(parent)?;
    Ok(canonical_parent.join(file_name))
}

/// Match a path against a glob pattern. Supports `*`, `**`, and `?`.
///
/// # Platform separator behaviour (evidence-based, not assumed)
///
/// `glob` 0.3.x uses `std::path::is_separator` throughout its matching code.
/// On Windows that function returns `true` for **both** `/` and `\`.
/// Concretely:
///
/// * `chars_eq(a, b, _)` (glob src line ~1041) short-circuits to `true`
///   whenever both characters satisfy `is_separator`, so `/` and `\` are
///   interchangeable in every character comparison.
/// * `AnyRecursiveSequence` (`**`) advances while `!follows_separator`; because
///   `\` flips `follows_separator` to `true` on Windows, `**` correctly bridges
///   backslash-separated path segments without any pre-normalisation.
///
/// Therefore: **no forward-slash normalisation is needed** on Windows.  Both
/// config patterns (expanded via `home.join(…)` → backslashes) and canonicalized
/// resolved paths (backslashes after `canonicalize_simplified`) are matched
/// correctly by the glob crate as-is.
///
/// # Case sensitivity
///
/// On Windows, NTFS is case-insensitive by default.  We mirror that by setting
/// `case_sensitive: false` on Windows.  All other `MatchOptions` preserve the
/// existing behaviour (`require_literal_separator: false`,
/// `require_literal_leading_dot: false`).
///
/// ## Non-ASCII case folding on Windows
///
/// The glob crate's `case_sensitive: false` only folds ASCII characters, so a
/// denylist pattern containing non-ASCII letters (é/É, ö/Ö, Cyrillic, etc.)
/// could be bypassed by a case variant on NTFS (which uses the full Unicode
/// upcase table).  To close this gap, on Windows we pre-lowercase **both** the
/// pattern and the path with Rust's Unicode-aware `str::to_lowercase` before
/// matching (and then match case-sensitively).  Rust's `to_lowercase` applies
/// Unicode simple case-fold (é→é, É→é, Ö→ö, Cyrillic, etc.), which is strictly
/// better than ASCII-only folding.  Note that this is Unicode simple-fold, not
/// full NTFS upcase-table parity, but for a denylist the security direction is
/// fail-closed: an unrecognised fold variant is still passed through (worst case
/// a denylist miss), whereas the old ASCII-only code guaranteed the bypass for
/// any non-ASCII case pair.  We keep `case_sensitive: false` as belt-and-
/// suspenders for the ASCII range.
fn glob_match(pattern: &str, path: &str) -> bool {
    #[cfg(windows)]
    {
        // Pre-fold both sides with Unicode-aware to_lowercase so that non-ASCII
        // case pairs (é/É, ö/Ö, Cyrillic, etc.) are folded before the glob
        // crate's ASCII-only case_sensitive:false matching runs over them.
        let pattern_lower = pattern.to_lowercase();
        let path_lower = path.to_lowercase();
        let opts = glob::MatchOptions {
            case_sensitive: false, // belt-and-suspenders for ASCII range
            require_literal_separator: false,
            require_literal_leading_dot: false,
        };
        match glob::Pattern::new(&pattern_lower) {
            Ok(p) => p.matches_with(&path_lower, opts),
            Err(_) => {
                // Pattern is invalid (e.g. unmatched `[`): fall back to
                // Unicode-folded equality.
                pattern_lower == path_lower
            }
        }
    }
    #[cfg(not(windows))]
    {
        let opts = glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: false,
            require_literal_leading_dot: false,
        };
        match glob::Pattern::new(pattern) {
            Ok(p) => p.matches_with(path, opts),
            Err(_) => {
                // Pattern is invalid (e.g. unmatched `[`): fall back to literal
                // equality so we don't silently allow/deny everything.
                pattern == path
            }
        }
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

    /// Canonicalize `tmp.path()` to its real path (macOS /var symlink handling)
    /// and strip the Windows verbatim prefix (`\\?\`) so the path can be used
    /// directly as a config pattern and compared with check_path results.
    fn canon_root(tmp: &TempDir) -> PathBuf {
        ahand_platform::paths::canonicalize_simplified(tmp.path()).unwrap()
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
        let err = checker
            .check_path(&link.to_string_lossy(), false, false)
            .unwrap_err();
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
        let err = checker
            .check_path(&key.to_string_lossy(), false, false)
            .unwrap_err();
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
        let result = checker
            .check_path(&bashrc.to_string_lossy(), false, false)
            .unwrap();
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
        let result = checker
            .check_path(&docs.to_string_lossy(), false, false)
            .unwrap();
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
        let err = checker
            .check_path(&file.to_string_lossy(), false, false)
            .unwrap_err();
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
        let err = checker
            .check_path(&file.to_string_lossy(), false, false)
            .unwrap_err();
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

    /// Regression test for the Windows verbatim-prefix policy bug: on
    /// Windows, std::fs::canonicalize returns \\?\-prefixed paths which can
    /// never glob-match the (unprefixed) config patterns, denying every file
    /// op. This test is the cross-platform pin: on unix it's trivially green;
    /// on windows CI it proves the dunce::simplify fix works.
    #[test]
    fn allowlisted_canonical_tempdir_passes_check_path() {
        let tmp = TempDir::new().unwrap();
        // Build the pattern the way operators do: from a plain
        // (non-verbatim) absolute path + /**.
        let root = ahand_platform::paths::canonicalize_simplified(tmp.path()).unwrap();
        let root_str = root.to_string_lossy().into_owned();
        let allowlist = vec![format!("{}/**", root_str.trim_end_matches('/')), root_str];
        let config = FilePolicyConfig {
            enabled: true,
            path_allowlist: allowlist,
            path_denylist: Vec::new(),
            max_read_bytes: 100_000_000,
            max_write_bytes: 100_000_000,
            dangerous_paths: Vec::new(),
        };
        let checker = FilePolicyChecker::new(&config);
        let file = root.join("hello.txt");
        std::fs::write(&file, b"hi").unwrap();
        let result = checker.check_path(&file.to_string_lossy(), false, false);
        assert!(
            result.is_ok(),
            "check_path denied an allowlisted path: {result:?}"
        );
    }

    /// canonicalize_or_parent must return simplified (non-verbatim) paths —
    /// the policy matcher's contract with ahand_platform::paths::simplify.
    #[test]
    fn canonicalize_or_parent_returns_simplified_existing_path() {
        let tmp = TempDir::new().unwrap();
        let got = canonicalize_or_parent(tmp.path()).unwrap();
        assert!(!got.to_string_lossy().starts_with(r"\\?\"));
        assert_eq!(got, ahand_platform::paths::simplify(&got));
    }

    /// Same contract for the not-yet-existing-suffix branch.
    #[test]
    fn canonicalize_or_parent_returns_simplified_for_missing_suffix() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("not").join("yet").join("here.txt");
        let got = canonicalize_or_parent(&missing).unwrap();
        assert!(!got.to_string_lossy().starts_with(r"\\?\"));
        assert!(got.ends_with("not/yet/here.txt") || got.ends_with(r"not\yet\here.txt"));
    }

    // -----------------------------------------------------------------------
    // T4: Case-correct policy matching (Task 4 of the M4 security plan)
    // -----------------------------------------------------------------------

    /// On Windows, glob_match must be case-insensitive so that NTFS paths
    /// whose on-disk casing differs from the config pattern are still matched.
    ///
    /// Separator note: glob 0.3.x treats both '/' and '\' as separators on
    /// Windows via std::path::is_separator, so no normalisation is required.
    /// Both ** and literal path segments match across backslash-separated paths.
    #[cfg(windows)]
    #[test]
    fn windows_glob_match_case_insensitive_allow() {
        // Pattern uses uppercase drive letter and mixed case; path is all lower.
        assert!(
            glob_match(r"C:\Users\X\**", r"c:\users\x\file.txt"),
            "case-insensitive allowlist match must succeed on Windows"
        );
        // Forward-slash pattern against backslash path (separator parity).
        assert!(
            glob_match("C:/Users/X/**", r"c:\users\x\file.txt"),
            "forward-slash pattern must match backslash path on Windows"
        );
    }

    /// Security-critical: a denylist entry must block case-variant paths on
    /// Windows. A user must not be able to bypass `.SSH` → `.ssh` denylist.
    #[cfg(windows)]
    #[test]
    fn windows_denylist_case_variant_blocked() {
        // This is the security invariant: `.ssh` denylist blocks `.SSH` path.
        assert!(
            glob_match(r"C:\Users\X\.ssh\**", r"c:\users\x\.SSH\id_rsa"),
            "denylist must block case-variant path on Windows (security)"
        );
        // Reversed: uppercase pattern must block lowercase path.
        assert!(
            glob_match(r"C:\Users\X\.SSH\**", r"c:\users\x\.ssh\id_rsa"),
            "denylist uppercase pattern must block lowercase path on Windows"
        );
    }

    /// On Windows, a mixed-case allowlist pattern must admit the canonical
    /// (OS-cased) resolved path produced by canonicalize_simplified.
    #[cfg(windows)]
    #[test]
    fn windows_mixed_case_allowlist_admits_canonical_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = ahand_platform::paths::canonicalize_simplified(tmp.path()).unwrap();
        let root_str = root.to_string_lossy().into_owned();

        // Build the allowlist pattern in uppercase (simulating operator config
        // that doesn't match the canonical casing on disk).
        let upper_pattern = root_str.to_uppercase();
        let allowlist = vec![
            format!("{}\\**", upper_pattern.trim_end_matches('\\')),
            upper_pattern,
        ];
        let config = crate::config::FilePolicyConfig {
            enabled: true,
            path_allowlist: allowlist,
            path_denylist: Vec::new(),
            max_read_bytes: 100_000_000,
            max_write_bytes: 100_000_000,
            dangerous_paths: Vec::new(),
        };
        let checker = FilePolicyChecker::new(&config);
        let file = root.join("test.txt");
        std::fs::write(&file, b"hi").unwrap();
        // The canonical path (exact on-disk casing) must be admitted by the
        // upper-cased allowlist pattern.
        let result = checker.check_path(&file.to_string_lossy(), false, false);
        assert!(
            result.is_ok(),
            "uppercase allowlist pattern must admit canonical path on Windows: {result:?}"
        );
    }

    /// Security-critical: prove that the denylist case-bypass is blocked through
    /// the REAL `check_path` flow (not just `glob_match` in isolation).
    ///
    /// Scenario: the operator's denylist uses the lowercase `.ssh` casing, but
    /// an attacker supplies a write destination with `.SSH` (uppercase).  Because
    /// the path does NOT yet exist, `canonicalize_or_parent` canonicalizes the
    /// existing parent and re-appends the attacker-cased suffix — so the resolved
    /// path retains `.SSH`.  The `glob_match` call with `case_sensitive: false`
    /// must still match the lowercase denylist pattern against the uppercase path
    /// and deny the request.
    ///
    /// The allowlist explicitly admits the parent directory so the denial is
    /// provably from the denylist, not from an allowlist miss.
    #[cfg(windows)]
    #[test]
    fn windows_denylist_case_bypass_blocked_through_check_path() {
        let tmp = TempDir::new().unwrap();
        let root = canon_root(&tmp);

        // Create the parent directory (.SSH does NOT exist — attacker casing).
        // The denylist pattern uses canonical lowercase (.ssh); the write
        // destination uses uppercase (.SSH).
        let ssh_dir_lower = root.join(".ssh");
        // Do NOT create .SSH — the path must be not-yet-existing so that
        // canonicalize_or_parent re-appends the attacker-cased suffix.

        // Denylist uses lowercase; allowlist admits the root so the parent
        // directory (root itself) is allowed — isolating the denylist denial.
        let denylist = vec![format!("{}/.ssh/**", root.display())];
        let checker = FilePolicyChecker::new(&cfg_for_tempdir(&root, &[], &denylist, &[]));

        // The write destination: .SSH (uppercase) / id_rsa — does not exist yet.
        // Use the lowercase ssh_dir_lower as base but suffix with uppercase variant.
        let root_str = root.to_string_lossy();
        let attacker_path = format!(r"{}\.SSH\id_rsa", root_str);

        let err = checker.check_path(&attacker_path, true, false).unwrap_err();
        assert_eq!(
            err.code,
            FileErrorCode::PolicyDenied as i32,
            "denylist must block case-variant path through check_path (not-yet-existing write): {err:?}"
        );

        // Suppress unused variable warning for ssh_dir_lower (intentionally not created).
        let _ = ssh_dir_lower;
    }

    /// Regression: on Unix, glob_match must remain case-SENSITIVE.
    /// Pattern `/Home/**` must NOT match `/home/x` (different capitalisation).
    #[cfg(unix)]
    #[test]
    fn unix_glob_match_remains_case_sensitive() {
        // Different casing → must NOT match on Unix.
        assert!(
            !glob_match("/Home/**", "/home/x"),
            "Unix glob must be case-sensitive: /Home/** must not match /home/x"
        );
        // Same casing → must match.
        assert!(
            glob_match("/home/**", "/home/x"),
            "Unix glob must match when casing is identical"
        );
        // Partial case difference in the middle of the path.
        assert!(
            !glob_match("/home/User/**", "/home/user/file.txt"),
            "Unix glob must be case-sensitive for inner components"
        );
    }

    /// Security: on Windows, non-ASCII case variants (é/É, ö/Ö) in denylist
    /// patterns must block the case-folded path variant.  The glob crate's
    /// `case_sensitive: false` only folds ASCII; our Unicode-aware pre-fold via
    /// `to_lowercase` closes the gap.  This is Unicode simple-fold (not full NTFS
    /// upcase-table parity), strictly better than ASCII-only — fail-closed for
    /// denylists.
    #[cfg(windows)]
    #[test]
    fn windows_glob_match_non_ascii_case_fold() {
        // é (U+00E9) / É (U+00C9): pattern uses uppercase É, path uses lowercase é.
        assert!(
            glob_match(r"C:\Users\ÉTÉ\**", r"C:\Users\été\file.txt"),
            "denylist pattern with É must block path with é (Unicode fold)"
        );
        // Reversed: lowercase pattern, uppercase path variant.
        assert!(
            glob_match(r"C:\Users\été\**", r"C:\Users\ÉTÉ\file.txt"),
            "denylist pattern with é must block path with É (Unicode fold)"
        );
        // ö (U+00F6) / Ö (U+00D6)
        assert!(
            glob_match(r"C:\Users\Öffnung\**", r"C:\Users\öffnung\secret.txt"),
            "denylist pattern with Ö must block path with ö (Unicode fold)"
        );
        // ASCII range must still work (belt-and-suspenders).
        assert!(
            glob_match(r"C:\Users\X\**", r"c:\users\x\file.txt"),
            "ASCII case-insensitive match must still work on Windows"
        );
    }
}
