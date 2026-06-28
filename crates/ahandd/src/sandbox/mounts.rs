use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use super::registry::SandboxSessionState;
use super::types::{
    MountAccess, MountSource, MountSourceSnapshot, RegisteredSandboxMount, SandboxError,
    SandboxMountSpec, SandboxResult,
};

pub fn register_mount(
    session: &SandboxSessionState,
    spec: SandboxMountSpec,
) -> SandboxResult<RegisteredSandboxMount> {
    if spec.access != MountAccess::ReadOnly {
        return Err(SandboxError::mount_access_denied(
            "sandbox mounts currently support read-only access only",
        ));
    }

    let workspace_root = session.workspace_root.canonicalize().map_err(|e| {
        SandboxError::mount_target_invalid(format!("failed to resolve sandbox workspace root: {e}"))
    })?;
    let mount_namespace = workspace_root.join("workspace").join("mounts");
    let canonical_namespace = mount_namespace.canonicalize().map_err(|e| {
        SandboxError::mount_target_invalid(format!("failed to resolve mount namespace: {e}"))
    })?;
    let (source, source_snapshot) = resolve_source(spec.source)?;
    let target = match spec.target {
        Some(target) => resolve_explicit_target(&workspace_root, &canonical_namespace, &target)?,
        None => allocate_auto_target(&canonical_namespace, &slug_from_mount_id(&spec.mount_id)),
    };

    Ok(RegisteredSandboxMount {
        mount_id: spec.mount_id,
        source,
        access: spec.access,
        scope: spec.scope,
        target,
        env_var: spec.env_var,
        source_snapshot,
    })
}

fn resolve_source(source: MountSource) -> SandboxResult<(MountSource, MountSourceSnapshot)> {
    match source {
        MountSource::HostPath(path) => {
            if is_url_like_path(&path) {
                return Err(SandboxError::mount_source_unsupported(
                    "URL-like mount sources are not supported",
                ));
            }
            let canonical = path.canonicalize().map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    SandboxError::mount_source_not_found(format!(
                        "mount source does not exist: {}",
                        path.display()
                    ))
                } else {
                    SandboxError::unavailable(format!(
                        "failed to resolve mount source '{}': {e}",
                        path.display()
                    ))
                }
            })?;
            let snapshot = snapshot_for_path(&canonical)?;
            Ok((MountSource::HostPath(canonical), snapshot))
        }
        other => Ok((
            other,
            MountSourceSnapshot {
                exists: false,
                is_dir: false,
            },
        )),
    }
}

fn snapshot_for_path(path: &Path) -> SandboxResult<MountSourceSnapshot> {
    let metadata = fs::metadata(path).map_err(|e| {
        SandboxError::unavailable(format!(
            "failed to read mount source metadata '{}': {e}",
            path.display()
        ))
    })?;
    Ok(MountSourceSnapshot {
        exists: true,
        is_dir: metadata.is_dir(),
    })
}

fn resolve_explicit_target(
    workspace_root: &Path,
    canonical_namespace: &Path,
    target: &Path,
) -> SandboxResult<PathBuf> {
    if target.is_absolute() {
        return Err(SandboxError::mount_target_invalid(
            "mount targets must be relative to the session root",
        ));
    }
    for component in target.components() {
        if matches!(
            component,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        ) {
            return Err(SandboxError::mount_target_invalid(
                "mount target contains disallowed traversal",
            ));
        }
    }

    let candidate = workspace_root.join(target);
    if !candidate.starts_with(canonical_namespace) {
        return Err(SandboxError::mount_target_invalid(
            "mount target must be under workspace/mounts",
        ));
    }

    if path_exists(&candidate) {
        let canonical_candidate = candidate.canonicalize().map_err(|e| {
            SandboxError::mount_target_invalid(format!(
                "failed to resolve mount target '{}': {e}",
                candidate.display()
            ))
        })?;
        if !canonical_candidate.starts_with(canonical_namespace) {
            return Err(SandboxError::mount_target_invalid(
                "mount target escapes workspace/mounts",
            ));
        }
        return Err(SandboxError::mount_target_conflict(format!(
            "mount target already exists: {}",
            candidate.display()
        )));
    }

    let nearest_existing = nearest_existing_ancestor(&candidate).ok_or_else(|| {
        SandboxError::mount_target_invalid("mount target has no existing parent namespace")
    })?;
    let canonical_existing = nearest_existing.canonicalize().map_err(|e| {
        SandboxError::mount_target_invalid(format!(
            "failed to resolve mount target parent '{}': {e}",
            nearest_existing.display()
        ))
    })?;
    if !canonical_existing.starts_with(canonical_namespace) {
        return Err(SandboxError::mount_target_invalid(
            "mount target parent escapes workspace/mounts",
        ));
    }

    Ok(candidate)
}

fn allocate_auto_target(canonical_namespace: &Path, slug: &str) -> PathBuf {
    let mut suffix = 1;
    loop {
        let name = if suffix == 1 {
            slug.to_string()
        } else {
            format!("{slug}-{suffix}")
        };
        let candidate = canonical_namespace.join(name);
        if !path_exists(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut current = path.parent();
    while let Some(candidate) = current {
        if path_exists(candidate) {
            return Some(candidate.to_path_buf());
        }
        current = candidate.parent();
    }
    None
}

fn slug_from_mount_id(mount_id: &str) -> String {
    let mut slug = String::new();
    let mut previous_separator = false;

    for ch in mount_id.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            previous_separator = false;
        } else if !previous_separator && !slug.is_empty() {
            slug.push('-');
            previous_separator = true;
        }
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    if slug.is_empty() {
        "mount".to_string()
    } else {
        slug
    }
}

fn is_url_like_path(path: &Path) -> bool {
    let raw = path.to_string_lossy();
    raw.contains("://")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::register_mount;
    use crate::sandbox::NetworkPolicy;
    use crate::sandbox::registry::SandboxSessionState;
    use crate::sandbox::types::{
        MountAccess, MountScope, MountSource, RegisteredSandboxMount, SandboxMountSpec,
        SandboxPermissionMode, SandboxSessionConfig,
    };

    fn session(workspace_root: &Path) -> SandboxSessionState {
        fs::create_dir_all(workspace_root.join("workspace").join("mounts")).unwrap();
        SandboxSessionState::from_config(SandboxSessionConfig {
            session_id: "session-1".to_string(),
            permission_mode: SandboxPermissionMode::Readonly,
            workspace_root: workspace_root.to_path_buf(),
            network: NetworkPolicy::Enabled,
            mounts: Vec::new(),
        })
    }

    fn readonly_host_spec(mount_id: &str, source: PathBuf) -> SandboxMountSpec {
        SandboxMountSpec {
            mount_id: mount_id.to_string(),
            source: MountSource::HostPath(source),
            access: MountAccess::ReadOnly,
            scope: MountScope::Run {
                run_id: "run-1".to_string(),
            },
            target: None,
            env_var: Some("COFFICE_SELECTED_FOLDER_DIR".to_string()),
        }
    }

    fn register_for_test(
        workspace_root: &Path,
        spec: SandboxMountSpec,
    ) -> crate::sandbox::SandboxResult<RegisteredSandboxMount> {
        register_mount(&session(workspace_root), spec)
    }

    #[test]
    fn auto_target_uses_mount_id_not_host_path() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host").join("Private Client");
        fs::create_dir_all(&source).unwrap();
        let source_with_parent = source.join("..").join("Private Client");

        let mount = register_for_test(
            &workspace_root,
            readonly_host_spec("selected-folder", source_with_parent),
        )
        .unwrap();

        assert_eq!(
            mount.target,
            workspace_root
                .canonicalize()
                .unwrap()
                .join("workspace/mounts/selected-folder")
        );
        assert!(!mount.target.to_string_lossy().contains("Private Client"));
        assert_eq!(
            mount.source,
            MountSource::HostPath(source.canonicalize().unwrap())
        );
        assert!(mount.source_snapshot.exists);
        assert!(mount.source_snapshot.is_dir);
    }

    #[test]
    fn missing_host_source_returns_not_found() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");

        let err = register_for_test(
            &workspace_root,
            readonly_host_spec("selected-folder", temp.path().join("missing")),
        )
        .unwrap_err();

        assert_eq!(err.code, "MOUNT_SOURCE_NOT_FOUND");
    }

    #[test]
    fn url_like_host_source_returns_unsupported() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");

        let err = register_for_test(
            &workspace_root,
            readonly_host_spec(
                "selected-folder",
                PathBuf::from("https://example.test/private-client"),
            ),
        )
        .unwrap_err();

        assert_eq!(err.code, "MOUNT_SOURCE_UNSUPPORTED");
    }

    #[test]
    fn non_read_only_access_returns_access_denied() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        fs::create_dir_all(&source).unwrap();

        for access in [
            MountAccess::WriteOnly,
            MountAccess::ReadWrite,
            MountAccess::CopyOnWrite,
        ] {
            let mut spec = readonly_host_spec("selected-folder", source.clone());
            spec.access = access;

            let err = register_for_test(&workspace_root, spec).unwrap_err();

            assert_eq!(err.code, "MOUNT_ACCESS_DENIED");
        }
    }

    #[test]
    fn explicit_target_outside_mount_namespace_returns_invalid() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        fs::create_dir_all(&source).unwrap();

        let mut spec = readonly_host_spec("selected-folder", source.clone());
        spec.target = Some(workspace_root.join("workspace").join("outside"));
        let err = register_for_test(&workspace_root, spec).unwrap_err();
        assert_eq!(err.code, "MOUNT_TARGET_INVALID");

        let mut spec = readonly_host_spec("selected-folder", source);
        spec.target = Some(PathBuf::from("workspace/mounts/../escape"));
        let err = register_for_test(&workspace_root, spec).unwrap_err();
        assert_eq!(err.code, "MOUNT_TARGET_INVALID");
    }

    #[cfg(unix)]
    #[test]
    fn explicit_target_symlink_escape_returns_invalid() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let namespace = workspace_root.join("workspace").join("mounts");
        let source = temp.path().join("host");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(&namespace).unwrap();
        symlink(&outside, namespace.join("link")).unwrap();

        let mut spec = readonly_host_spec("selected-folder", source);
        spec.target = Some(PathBuf::from("workspace/mounts/link/selected-folder"));

        let err = register_for_test(&workspace_root, spec).unwrap_err();

        assert_eq!(err.code, "MOUNT_TARGET_INVALID");
    }

    #[test]
    fn explicit_target_conflict_returns_conflict() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        let target = workspace_root
            .join("workspace")
            .join("mounts")
            .join("selected-folder");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();

        let mut spec = readonly_host_spec("selected-folder", source);
        spec.target = Some(PathBuf::from("workspace/mounts/selected-folder"));

        let err = register_for_test(&workspace_root, spec).unwrap_err();

        assert_eq!(err.code, "MOUNT_TARGET_CONFLICT");
    }

    #[test]
    fn auto_target_uses_deterministic_suffixes_for_existing_paths() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let namespace = workspace_root.join("workspace").join("mounts");
        let source = temp.path().join("host");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(namespace.join("selected-folder")).unwrap();
        fs::create_dir_all(namespace.join("selected-folder-2")).unwrap();

        let mount = register_for_test(
            &workspace_root,
            readonly_host_spec("selected-folder", source),
        )
        .unwrap();

        assert_eq!(
            mount.target,
            workspace_root
                .canonicalize()
                .unwrap()
                .join("workspace/mounts/selected-folder-3")
        );
    }
}
