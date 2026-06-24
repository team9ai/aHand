//! Windows path conversion helpers for Win32 calls.
#![cfg_attr(not(test), allow(dead_code))]

use std::io;
use std::path::{Path, PathBuf};

pub(super) fn absolute(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

pub(super) fn wide_null(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

pub(super) fn string_wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_wide_null_appends_terminator() {
        assert_eq!(string_wide_null("NUL"), vec![78, 85, 76, 0]);
    }

    #[test]
    fn path_wide_null_appends_terminator() {
        assert_eq!(wide_null(Path::new("NUL")), vec![78, 85, 76, 0]);
    }

    #[test]
    fn absolute_keeps_absolute_paths() {
        let temp = tempfile::tempdir().unwrap();

        assert_eq!(absolute(temp.path()).unwrap(), temp.path());
    }

    #[test]
    fn absolute_resolves_relative_paths_against_current_dir() {
        let resolved = absolute(Path::new("Cargo.toml")).unwrap();

        assert!(resolved.is_absolute());
        assert!(resolved.ends_with("Cargo.toml"));
    }
}
