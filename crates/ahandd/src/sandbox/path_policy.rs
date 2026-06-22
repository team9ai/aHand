use std::path::{Component, Path, PathBuf};

use super::types::{SandboxError, SandboxResult};

pub fn resolve_existing_sandbox_path(root: &Path, relative_path: &str) -> SandboxResult<PathBuf> {
    let candidate = sandbox_candidate(root, relative_path)?;
    let canonical_root = root.canonicalize().map_err(|e| {
        SandboxError::invalid_sandbox_path(format!("failed to resolve sandbox root: {e}"))
    })?;
    let canonical_candidate = candidate.canonicalize().map_err(|e| {
        SandboxError::invalid_sandbox_path(format!("failed to resolve sandbox path: {e}"))
    })?;

    if !canonical_candidate.starts_with(&canonical_root) {
        return Err(SandboxError::invalid_sandbox_path(
            "sandbox path escapes session root",
        ));
    }

    Ok(canonical_candidate)
}

pub fn resolve_new_sandbox_path(root: &Path, relative_path: &str) -> SandboxResult<PathBuf> {
    let candidate = sandbox_candidate(root, relative_path)?;
    let parent = candidate.parent().ok_or_else(|| {
        SandboxError::invalid_sandbox_path("sandbox path has no parent directory")
    })?;
    let relative_parent = parent.strip_prefix(root).unwrap_or(parent);
    let canonical_parent = resolve_existing_sandbox_path(root, &relative_parent.to_string_lossy())?;
    let file_name = candidate
        .file_name()
        .ok_or_else(|| SandboxError::invalid_sandbox_path("sandbox path has no file name"))?;

    Ok(canonical_parent.join(file_name))
}

fn sandbox_candidate(root: &Path, relative_path: &str) -> SandboxResult<PathBuf> {
    let rel = Path::new(relative_path);
    if rel.is_absolute() {
        return Err(SandboxError::invalid_sandbox_path(
            "sandbox paths must be relative to the session root",
        ));
    }
    for component in rel.components() {
        if matches!(
            component,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        ) {
            return Err(SandboxError::invalid_sandbox_path(
                "sandbox path contains disallowed traversal",
            ));
        }
    }

    Ok(root.join(rel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn sandbox_path_must_stay_inside_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sandbox");
        fs::create_dir_all(root.join("workspace")).unwrap();
        fs::write(root.join("workspace/out.txt"), "ok").unwrap();

        let inside = resolve_existing_sandbox_path(&root, "workspace/out.txt").unwrap();
        let outside = resolve_existing_sandbox_path(&root, "../outside.txt").unwrap_err();

        assert!(inside.ends_with("workspace/out.txt"));
        assert_eq!(outside.code, "INVALID_SANDBOX_PATH");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sandbox");
        let outside = temp.path().join("outside");
        fs::create_dir_all(root.join("workspace")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "secret").unwrap();
        symlink(outside.join("secret.txt"), root.join("workspace/link.txt")).unwrap();

        let err = resolve_existing_sandbox_path(&root, "workspace/link.txt").unwrap_err();

        assert_eq!(err.code, "INVALID_SANDBOX_PATH");
    }

    #[test]
    fn new_sandbox_path_uses_existing_parent_inside_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sandbox");
        fs::create_dir_all(root.join("workspace")).unwrap();

        let resolved = resolve_new_sandbox_path(&root, "workspace/new.txt").unwrap();

        assert_eq!(
            resolved,
            root.canonicalize().unwrap().join("workspace/new.txt")
        );
    }

    #[cfg(unix)]
    #[test]
    fn new_sandbox_path_rejects_symlinked_parent_escape() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sandbox");
        let outside = temp.path().join("outside");
        fs::create_dir_all(root.join("workspace")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("workspace/link")).unwrap();

        let err = resolve_new_sandbox_path(&root, "workspace/link/new.txt").unwrap_err();

        assert_eq!(err.code, "INVALID_SANDBOX_PATH");
    }
}
