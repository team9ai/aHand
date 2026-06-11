//! Path helpers that make Windows paths behave like Unix paths for the rest
//! of the codebase: executable naming and verbatim-prefix (`\\?\`) stripping.

use std::io;
use std::path::{Path, PathBuf};

/// Append the platform executable suffix (`.exe` on Windows).
pub fn exe_name(base: &str) -> String {
    #[cfg(windows)]
    {
        format!("{base}.exe")
    }
    #[cfg(not(windows))]
    {
        base.to_string()
    }
}

/// Strip the Windows verbatim disk prefix (`\\?\C:\...`) so the result is
/// comparable with user-written config patterns. Verbatim UNC paths
/// (`\\?\UNC\...`) are intentionally left unchanged (dunce semantics, legacy
/// consumers can't take bare UNC). Identity on Unix.
pub fn simplify(path: &Path) -> PathBuf {
    dunce::simplified(path).to_path_buf()
}

/// `std::fs::canonicalize` + [`simplify`]. Use this INSTEAD of raw
/// `canonicalize` anywhere the result is string-compared or glob-matched
/// (policy allow/deny lists), or shown to users.
pub fn canonicalize_simplified(path: &Path) -> io::Result<PathBuf> {
    Ok(simplify(&std::fs::canonicalize(path)?))
}

// --- Release asset suffix --------------------------------------------------

/// Release asset suffix for the current platform, matching the `suffix:`
/// values in `.github/workflows/release-rust.yml`.
///
/// Workflow matrix (pinned):
///   linux-x64   | x86_64-unknown-linux-gnu
///   linux-arm64 | aarch64-unknown-linux-gnu  (cross)
///   darwin-arm64 | aarch64-apple-darwin
///   darwin-x64   | x86_64-apple-darwin
///   windows-x64  | x86_64-pc-windows-msvc
///
/// Binaries are named `ahandd-{suffix}[.exe]` and `ahandctl-{suffix}[.exe]`;
/// checksums: `checksums-rust-{suffix}.txt`.
pub fn release_suffix() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other, // "linux", "windows" already match release naming
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("{os}-{arch}")
}

// --- Version marker --------------------------------------------------------

/// Path of the version marker file inside the aHand home dir.
pub fn version_file(ahand_home: &Path) -> PathBuf {
    ahand_home.join("version")
}

/// Read the installed-version marker (trimmed); `None` if absent or
/// whitespace-only.
pub fn read_version_marker(ahand_home: &Path) -> Option<String> {
    let content = std::fs::read_to_string(version_file(ahand_home)).ok()?;
    let trimmed = content.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Write the version marker (stored trimmed + trailing newline for parity with
/// shell scripts).  Creates `ahand_home` if it does not exist.
pub fn write_version_marker(ahand_home: &Path, version: &str) -> io::Result<()> {
    std::fs::create_dir_all(ahand_home)?;
    std::fs::write(version_file(ahand_home), format!("{}\n", version.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_name_appends_exe_only_on_windows() {
        let n = exe_name("ahandd");
        #[cfg(windows)]
        assert_eq!(n, "ahandd.exe");
        #[cfg(not(windows))]
        assert_eq!(n, "ahandd");
    }

    #[test]
    fn simplify_is_identity_for_plain_paths() {
        let p = std::path::Path::new("/a/b");
        assert_eq!(simplify(p), std::path::PathBuf::from("/a/b"));
    }

    #[test]
    fn simplify_strips_verbatim_prefix_on_windows() {
        // On Unix this is a no-op path with backslashes in the file name —
        // only assert the Windows behavior under cfg(windows).
        #[cfg(windows)]
        {
            let p = std::path::Path::new(r"\\?\C:\Users\x");
            assert_eq!(simplify(p), std::path::PathBuf::from(r"C:\Users\x"));
        }
    }

    #[test]
    fn simplify_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let canon = std::fs::canonicalize(tmp.path()).unwrap();
        let once = simplify(&canon);
        assert_eq!(simplify(&once), once);
        // An already-simplified path round-trips unchanged.
        assert_eq!(simplify(&once), once);
    }

    #[test]
    fn canonicalize_simplified_has_no_verbatim_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let c = canonicalize_simplified(tmp.path()).unwrap();
        assert!(!c.to_string_lossy().starts_with(r"\\?\"));
        assert!(c.is_absolute());
    }

    // --- release_suffix tests -----------------------------------------------

    #[test]
    fn release_suffix_matches_current_platform() {
        let s = release_suffix();
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        assert_eq!(s, "darwin-arm64");
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        assert_eq!(s, "darwin-x64");
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        assert_eq!(s, "linux-x64");
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        assert_eq!(s, "linux-arm64");
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        assert_eq!(s, "windows-x64");
    }

    #[test]
    fn release_suffix_format_is_os_dash_arch() {
        // Structural sanity: must contain exactly one '-' separating two
        // non-empty parts.
        let s = release_suffix();
        let parts: Vec<&str> = s.splitn(2, '-').collect();
        assert_eq!(parts.len(), 2, "suffix must be 'os-arch', got: {s}");
        assert!(!parts[0].is_empty(), "os part must not be empty");
        assert!(!parts[1].is_empty(), "arch part must not be empty");
    }

    // --- version marker tests -----------------------------------------------

    #[test]
    fn version_marker_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        write_version_marker(tmp.path(), "1.2.3\n").unwrap();
        assert_eq!(read_version_marker(tmp.path()), Some("1.2.3".to_string()));
    }

    #[test]
    fn version_marker_round_trips_without_trailing_newline() {
        let tmp = tempfile::tempdir().unwrap();
        write_version_marker(tmp.path(), "1.2.3").unwrap();
        assert_eq!(read_version_marker(tmp.path()), Some("1.2.3".to_string()));
    }

    #[test]
    fn version_marker_missing_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(read_version_marker(tmp.path()), None);
    }

    #[test]
    fn version_marker_empty_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(version_file(tmp.path()), "").unwrap();
        assert_eq!(read_version_marker(tmp.path()), None);
    }

    #[test]
    fn version_marker_whitespace_only_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(version_file(tmp.path()), "   \n  \t  \n").unwrap();
        assert_eq!(read_version_marker(tmp.path()), None);
    }

    #[test]
    fn version_marker_creates_parent_dir_if_missing() {
        let tmp = tempfile::tempdir().unwrap();
        // Use a subdirectory that does not exist yet.
        let home = tmp.path().join("nested").join("ahand");
        assert!(!home.exists());
        write_version_marker(&home, "0.9.0").unwrap();
        assert!(home.exists());
        assert_eq!(read_version_marker(&home), Some("0.9.0".to_string()));
    }

    #[test]
    fn version_file_path_is_version_in_home() {
        let p = std::path::Path::new("/home/user/.ahand");
        assert_eq!(
            version_file(p),
            std::path::PathBuf::from("/home/user/.ahand/version")
        );
    }
}
