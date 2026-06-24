//! Windows filesystem root derivation for sandbox ACL setup.
#![cfg_attr(not(test), allow(dead_code))]

use std::path::{Path, PathBuf};

use crate::sandbox::runner::RuntimeSandboxPolicy;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FilesystemRoots {
    pub(super) read_roots: Vec<PathBuf>,
    pub(super) write_roots: Vec<PathBuf>,
}

pub(super) fn derive_filesystem_roots(
    policy: &RuntimeSandboxPolicy,
    state_root: &Path,
) -> FilesystemRoots {
    let mut write_roots = canonical_existing(std::slice::from_ref(&policy.writable_root));
    filter_sensitive_state_roots(&mut write_roots, state_root);
    let write_keys = write_roots
        .iter()
        .map(|root| path_key(root))
        .collect::<Vec<_>>();

    let mut read_roots = canonical_existing(&policy.readonly_roots);
    read_roots.retain(|root| !write_keys.iter().any(|key| *key == path_key(root)));
    filter_sensitive_state_roots(&mut read_roots, state_root);

    FilesystemRoots {
        read_roots,
        write_roots,
    }
}

fn canonical_existing(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = Vec::<String>::new();
    let mut out = Vec::new();
    for path in paths {
        if !path.exists() {
            continue;
        }
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        let key = path_key(&canonical);
        if seen.iter().any(|existing| existing == &key) {
            continue;
        }
        seen.push(key);
        out.push(canonical);
    }
    out
}

fn filter_sensitive_state_roots(roots: &mut Vec<PathBuf>, state_root: &Path) {
    let sensitive = [
        state_root.to_path_buf(),
        super::setup::sandbox_dir(state_root),
        super::setup::sandbox_secrets_dir(state_root),
    ]
    .into_iter()
    .filter(|path| path.exists())
    .map(|path| path.canonicalize().unwrap_or(path))
    .map(|path| path_key(&path))
    .collect::<Vec<_>>();

    roots.retain(|root| {
        let key = path_key(root);
        !sensitive
            .iter()
            .any(|parent| key == *parent || key.starts_with(&format!("{parent}/")))
    });
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use crate::sandbox::types::NetworkPolicy;

    use super::*;

    fn policy(writable_root: PathBuf, readonly_roots: Vec<PathBuf>) -> RuntimeSandboxPolicy {
        RuntimeSandboxPolicy {
            writable_root,
            readonly_roots,
            network: NetworkPolicy::Enabled,
        }
    }

    #[test]
    fn write_roots_include_existing_writable_root() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let state_root = temp.path().join("state");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&state_root).unwrap();

        let roots = derive_filesystem_roots(&policy(workspace.clone(), vec![]), &state_root);

        assert_eq!(roots.write_roots, vec![workspace.canonicalize().unwrap()]);
        assert!(roots.read_roots.is_empty());
    }

    #[test]
    fn read_roots_include_existing_readonly_roots_except_write_roots() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let runtime = temp.path().join("runtime");
        let state_root = temp.path().join("state");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::create_dir_all(&state_root).unwrap();

        let roots = derive_filesystem_roots(
            &policy(
                workspace.clone(),
                vec![
                    runtime.clone(),
                    workspace.clone(),
                    temp.path().join("missing"),
                ],
            ),
            &state_root,
        );

        assert_eq!(roots.write_roots, vec![workspace.canonicalize().unwrap()]);
        assert_eq!(roots.read_roots, vec![runtime.canonicalize().unwrap()]);
    }

    #[test]
    fn write_roots_filter_sandbox_state_and_secrets() {
        let temp = tempfile::tempdir().unwrap();
        let state_root = temp.path().join("state");
        let sandbox_dir = super::super::setup::sandbox_dir(&state_root);
        let secrets_dir = super::super::setup::sandbox_secrets_dir(&state_root);
        std::fs::create_dir_all(&sandbox_dir).unwrap();
        std::fs::create_dir_all(&secrets_dir).unwrap();

        let roots = derive_filesystem_roots(&policy(secrets_dir, vec![]), &state_root);

        assert!(roots.write_roots.is_empty());
    }
}
