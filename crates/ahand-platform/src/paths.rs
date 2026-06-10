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

/// Strip Windows verbatim prefixes (`\\?\`, `\\?\UNC\`) so the result is
/// comparable with user-written config patterns. Identity on Unix.
pub fn simplify(path: &Path) -> PathBuf {
    dunce::simplified(path).to_path_buf()
}

/// `std::fs::canonicalize` + [`simplify`]. Use this INSTEAD of raw
/// `canonicalize` anywhere the result is string-compared or glob-matched
/// (policy allow/deny lists), or shown to users.
pub fn canonicalize_simplified(path: &Path) -> io::Result<PathBuf> {
    Ok(simplify(&std::fs::canonicalize(path)?))
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
    fn canonicalize_simplified_has_no_verbatim_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let c = canonicalize_simplified(tmp.path()).unwrap();
        assert!(!c.to_string_lossy().starts_with(r"\\?\"));
        assert!(c.is_absolute());
    }
}
