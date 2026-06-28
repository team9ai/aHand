use std::collections::HashMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use super::platform;
use super::types::{
    MountAccess, MountSource, NetworkPolicy, RegisteredSandboxMount, RuntimeExecuteResult,
    RuntimeProviderConfig, SandboxError, SandboxResult,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSandboxPolicy {
    pub writable_root: PathBuf,
    pub readonly_roots: Vec<PathBuf>,
    pub mounts: Vec<RegisteredSandboxMount>,
    pub network: NetworkPolicy,
}

impl RuntimeSandboxPolicy {
    pub fn new(
        writable_root: PathBuf,
        provider: RuntimeProviderConfig,
        network: NetworkPolicy,
    ) -> Self {
        Self {
            writable_root,
            readonly_roots: provider.readonly_roots,
            mounts: Vec::new(),
            network,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformExecuteRequest {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    pub timeout: Duration,
    pub policy: RuntimeSandboxPolicy,
}

pub async fn execute(mut request: PlatformExecuteRequest) -> SandboxResult<RuntimeExecuteResult> {
    materialize_active_mounts(&mut request.policy)?;
    platform::execute(request).await
}

fn materialize_active_mounts(policy: &mut RuntimeSandboxPolicy) -> SandboxResult<()> {
    materialize_active_mounts_with_platform_support(
        policy,
        platform_supports_readonly_host_directory_mounts(),
    )
}

fn platform_supports_readonly_host_directory_mounts() -> bool {
    cfg!(target_os = "macos")
}

fn materialize_active_mounts_with_platform_support(
    policy: &mut RuntimeSandboxPolicy,
    platform_supported: bool,
) -> SandboxResult<()> {
    if policy.mounts.is_empty() {
        return Ok(());
    }
    if !platform_supported {
        return Err(SandboxError::mount_platform_unsupported(
            "read-only host directory mounts are not supported on this platform",
        ));
    }

    for mount in policy.mounts.clone() {
        let canonical_source = materialize_readonly_host_directory_mount(policy, &mount)?;
        push_unique_path(&mut policy.readonly_roots, canonical_source);
    }
    policy.readonly_roots.sort();
    policy.readonly_roots.dedup();
    Ok(())
}

fn materialize_readonly_host_directory_mount(
    policy: &RuntimeSandboxPolicy,
    mount: &RegisteredSandboxMount,
) -> SandboxResult<PathBuf> {
    if mount.access != MountAccess::ReadOnly {
        return Err(SandboxError::mount_platform_unsupported(
            "only read-only host directory mounts can be materialized",
        ));
    }
    if !mount.source_snapshot.exists {
        return Err(SandboxError::mount_source_not_found(format!(
            "mount source no longer exists for '{}'",
            mount.mount_id
        )));
    }
    if !mount.source_snapshot.is_dir {
        return Err(SandboxError::mount_source_unsupported(
            "only host directory mounts can be materialized",
        ));
    }

    let source = match &mount.source {
        MountSource::HostPath(path) => path,
        MountSource::SandboxPath(_) | MountSource::RuntimePath(_) => {
            return Err(SandboxError::mount_platform_unsupported(
                "only host path mounts can be materialized",
            ));
        }
    };
    let canonical_source = source.canonicalize().map_err(|e| {
        SandboxError::mount_source_not_found(format!(
            "failed to resolve mount source '{}': {e}",
            source.display()
        ))
    })?;
    let metadata = fs::metadata(&canonical_source).map_err(|e| {
        SandboxError::mount_source_not_found(format!(
            "failed to inspect mount source '{}': {e}",
            canonical_source.display()
        ))
    })?;
    if !metadata.is_dir() {
        return Err(SandboxError::mount_source_unsupported(
            "only host directory mounts can be materialized",
        ));
    }

    materialize_mount_target_symlink(&policy.writable_root, &mount.target, &canonical_source)?;
    Ok(canonical_source)
}

fn materialize_mount_target_symlink(
    workspace_root: &Path,
    target: &Path,
    canonical_source: &Path,
) -> SandboxResult<()> {
    let canonical_workspace = workspace_root.canonicalize().map_err(|e| {
        SandboxError::mount_target_invalid(format!("failed to resolve sandbox workspace root: {e}"))
    })?;
    let canonical_namespace = prepare_mount_namespace(&canonical_workspace)?;
    if !target.is_absolute() || contains_parent_component(target) {
        return Err(SandboxError::mount_target_invalid(
            "resolved mount target must be an absolute path without traversal",
        ));
    }
    let parent = validate_mount_target_parent(&canonical_namespace, target)?;

    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let link_target = fs::read_link(target).map_err(|e| {
                SandboxError::mount_target_invalid(format!(
                    "failed to read mount target symlink '{}': {e}",
                    target.display()
                ))
            })?;
            let resolved_link = if link_target.is_absolute() {
                link_target
            } else {
                parent.join(link_target)
            }
            .canonicalize()
            .map_err(|e| {
                SandboxError::mount_target_invalid(format!(
                    "failed to resolve mount target symlink '{}': {e}",
                    target.display()
                ))
            })?;
            if resolved_link == canonical_source {
                return Ok(());
            }
            return Err(SandboxError::mount_target_conflict(format!(
                "mount target already points elsewhere: {}",
                target.display()
            )));
        }
        Ok(_) => {
            return Err(SandboxError::mount_target_conflict(format!(
                "mount target already exists: {}",
                target.display()
            )));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(SandboxError::mount_target_invalid(format!(
                "failed to inspect mount target '{}': {e}",
                target.display()
            )));
        }
    }

    validate_mount_target_parent(&canonical_namespace, target)?;
    create_symlink(canonical_source, target)
}

fn prepare_mount_namespace(canonical_workspace: &Path) -> SandboxResult<PathBuf> {
    ensure_existing_plain_directory(canonical_workspace, "sandbox workspace root")?;
    let workspace_dir =
        ensure_plain_child_directory(canonical_workspace, "workspace", "sandbox workspace")?;
    let mount_namespace =
        ensure_plain_child_directory(&workspace_dir, "mounts", "mount namespace")?;
    let canonical_namespace = mount_namespace.canonicalize().map_err(|e| {
        SandboxError::mount_target_invalid(format!("failed to resolve mount namespace: {e}"))
    })?;
    if canonical_namespace != mount_namespace {
        return Err(SandboxError::mount_target_invalid(
            "mount namespace must resolve without symlinks",
        ));
    }
    Ok(canonical_namespace)
}

fn ensure_plain_child_directory(parent: &Path, child: &str, label: &str) -> SandboxResult<PathBuf> {
    ensure_existing_plain_directory(parent, &format!("{label} parent"))?;
    let path = parent.join(child);
    match fs::symlink_metadata(&path) {
        Ok(_) => ensure_existing_plain_directory(&path, label)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            ensure_existing_plain_directory(parent, &format!("{label} parent"))?;
            match fs::create_dir(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => {
                    return Err(SandboxError::mount_target_invalid(format!(
                        "failed to create {label}: {e}"
                    )));
                }
            }
            ensure_existing_plain_directory(&path, label)?;
        }
        Err(e) => {
            return Err(SandboxError::mount_target_invalid(format!(
                "failed to inspect {label}: {e}"
            )));
        }
    }
    Ok(path)
}

fn ensure_existing_plain_directory(path: &Path, label: &str) -> SandboxResult<()> {
    let metadata = fs::symlink_metadata(path).map_err(|e| {
        SandboxError::mount_target_invalid(format!("failed to inspect {label}: {e}"))
    })?;
    if metadata.file_type().is_symlink() {
        return Err(SandboxError::mount_target_invalid(format!(
            "{label} must not be a symlink"
        )));
    }
    if !metadata.is_dir() {
        return Err(SandboxError::mount_target_invalid(format!(
            "{label} must be a directory"
        )));
    }
    Ok(())
}

fn validate_mount_target_parent(
    canonical_namespace: &Path,
    target: &Path,
) -> SandboxResult<PathBuf> {
    let parent = target.parent().ok_or_else(|| {
        SandboxError::mount_target_invalid("mount target has no parent namespace")
    })?;
    ensure_existing_plain_directory(parent, "mount target parent")?;
    let file_name = target.file_name().ok_or_else(|| {
        SandboxError::mount_target_invalid("mount target must include a final path component")
    })?;
    let canonical_parent = parent.canonicalize().map_err(|e| {
        SandboxError::mount_target_invalid(format!(
            "failed to resolve mount target parent '{}': {e}",
            parent.display()
        ))
    })?;
    let canonical_target_path = canonical_parent.join(file_name);
    if !canonical_target_path.starts_with(canonical_namespace) {
        return Err(SandboxError::mount_target_invalid(
            "mount target parent escapes workspace/mounts",
        ));
    }
    Ok(parent.to_path_buf())
}

#[cfg(unix)]
fn create_symlink(source: &Path, target: &Path) -> SandboxResult<()> {
    symlink(source, target).map_err(|e| {
        SandboxError::mount_target_invalid(format!(
            "failed to create mount target symlink '{}': {e}",
            target.display()
        ))
    })
}

#[cfg(not(unix))]
fn create_symlink(_source: &Path, _target: &Path) -> SandboxResult<()> {
    Err(SandboxError::mount_platform_unsupported(
        "read-only host directory mounts require symlink support",
    ))
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

pub fn resolve_executable(program: &str, path_entries: &[PathBuf]) -> SandboxResult<PathBuf> {
    if program.trim().is_empty() {
        return Err(SandboxError::invalid_command(
            "command program must not be empty",
        ));
    }

    let program_path = PathBuf::from(program);
    if program_path.is_absolute() {
        if contains_parent_component(&program_path) {
            return Err(SandboxError::invalid_command(format!(
                "absolute sandbox command '{}' must not contain parent components",
                program
            )));
        }
        let is_registered_entry_path = path_entries
            .iter()
            .any(|entry| program_path.starts_with(entry));
        let resolved = program_path.canonicalize().map_err(|e| {
            SandboxError::command_not_found(format!(
                "failed to resolve sandbox command '{}': {e}",
                program
            ))
        })?;
        if is_registered_entry_path {
            return Ok(program_path);
        }
        if path_entries.iter().any(|entry| resolved.starts_with(entry)) {
            return Ok(resolved);
        }
        return Err(SandboxError::invalid_command(format!(
            "absolute sandbox command '{}' is outside registered runtime PATH",
            program
        )));
    }

    if program.contains('/') || program.contains('\\') {
        return Err(SandboxError::invalid_command(format!(
            "relative command paths are not allowed: {program}"
        )));
    }

    for entry in path_entries {
        let candidate = entry.join(program);
        if candidate.exists() {
            candidate.canonicalize().map_err(|e| {
                SandboxError::command_not_found(format!(
                    "failed to resolve sandbox command '{}': {e}",
                    candidate.display()
                ))
            })?;
            return Ok(candidate);
        }
    }

    Err(SandboxError::command_not_found(format!(
        "sandbox command '{program}' was not found in registered runtime PATH"
    )))
}

fn contains_parent_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::types::{
        MountAccess, MountScope, MountSource, MountSourceSnapshot, NetworkPolicy,
        RegisteredSandboxMount, RuntimeProviderConfig,
    };
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn runtime_policy_contains_session_write_root_and_readonly_roots() {
        let provider = RuntimeProviderConfig {
            name: "python".into(),
            executable: PathBuf::from("/runtimes/python/bin/python"),
            readonly_roots: vec![PathBuf::from("/runtimes/python")],
            env: HashMap::new(),
            default_timeout: Duration::from_secs(30),
        };
        let policy = RuntimeSandboxPolicy::new(
            PathBuf::from("/sessions/s1"),
            provider,
            NetworkPolicy::Enabled,
        );

        assert_eq!(policy.writable_root, PathBuf::from("/sessions/s1"));
        assert_eq!(
            policy.readonly_roots,
            vec![PathBuf::from("/runtimes/python")]
        );
        assert_eq!(policy.network, NetworkPolicy::Enabled);
    }

    #[test]
    fn resolve_executable_finds_bare_command_in_registered_path_entries() {
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let python = bin.join("python");
        std::fs::write(&python, "").unwrap();

        let resolved = resolve_executable("python", std::slice::from_ref(&bin)).unwrap();

        assert_eq!(resolved, python);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_executable_preserves_registered_bare_symlink_entry() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin");
        let target_dir = temp.path().join("target");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&target_dir).unwrap();
        let target_program = target_dir.join("python3");
        std::fs::write(&target_program, "").unwrap();
        let alias = bin.join("python");
        symlink(&target_program, &alias).unwrap();

        let resolved = resolve_executable("python", std::slice::from_ref(&bin)).unwrap();

        assert_eq!(resolved, alias);
    }

    #[test]
    fn resolve_executable_rejects_unknown_bare_command() {
        let temp = tempfile::tempdir().unwrap();
        let err = resolve_executable("python", &[temp.path().to_path_buf()]).unwrap_err();

        assert_eq!(err.code, "COMMAND_NOT_FOUND");
    }

    #[test]
    fn resolve_executable_rejects_relative_program_paths() {
        let err = resolve_executable("./python", &[PathBuf::from("/bin")]).unwrap_err();

        assert_eq!(err.code, "INVALID_COMMAND");
    }

    #[test]
    fn resolve_executable_rejects_absolute_program_outside_registered_path_entries() {
        let temp = tempfile::tempdir().unwrap();
        let allowed = temp.path().join("allowed");
        let denied = temp.path().join("denied");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&denied).unwrap();
        let denied_program = denied.join("python");
        std::fs::write(&denied_program, "").unwrap();

        let err = resolve_executable(&denied_program.to_string_lossy(), &[allowed]).unwrap_err();

        assert_eq!(err.code, "INVALID_COMMAND");
    }

    #[cfg(unix)]
    #[test]
    fn resolve_executable_allows_absolute_alias_under_registered_path_entry() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin");
        let target_dir = temp.path().join("target");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&target_dir).unwrap();
        let target_program = target_dir.join("python3");
        std::fs::write(&target_program, "").unwrap();
        let alias = bin.join("python");
        symlink(&target_program, &alias).unwrap();

        let resolved = resolve_executable(&alias.to_string_lossy(), &[bin]).unwrap();

        assert_eq!(resolved, alias);
    }

    #[test]
    fn resolve_executable_rejects_absolute_program_with_parent_components() {
        let temp = tempfile::tempdir().unwrap();
        let allowed = temp.path().join("allowed");
        let denied = temp.path().join("denied");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&denied).unwrap();
        std::fs::write(denied.join("python"), "").unwrap();
        let traversal = allowed.join("..").join("denied").join("python");

        let err = resolve_executable(&traversal.to_string_lossy(), &[allowed]).unwrap_err();

        assert_eq!(err.code, "INVALID_COMMAND");
    }

    #[tokio::test]
    async fn unsupported_platform_fails_closed() {
        let request = PlatformExecuteRequest {
            executable: PathBuf::from("/bin/echo"),
            args: vec!["hello".into()],
            cwd: PathBuf::from("/tmp"),
            env: HashMap::new(),
            timeout: Duration::from_secs(1),
            policy: RuntimeSandboxPolicy {
                writable_root: PathBuf::from("/tmp"),
                readonly_roots: vec![],
                mounts: Vec::new(),
                network: NetworkPolicy::Enabled,
            },
        };

        let err = platform::unsupported::execute(request).await.unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
    }

    fn readonly_host_dir_mount(
        mount_id: &str,
        source: PathBuf,
        target: PathBuf,
    ) -> RegisteredSandboxMount {
        RegisteredSandboxMount {
            mount_id: mount_id.to_string(),
            source: MountSource::HostPath(source),
            access: MountAccess::ReadOnly,
            scope: MountScope::Session,
            target,
            env_var: None,
            source_snapshot: MountSourceSnapshot {
                exists: true,
                is_dir: true,
            },
        }
    }

    fn policy_with_mount(
        workspace_root: PathBuf,
        source: PathBuf,
        target: PathBuf,
    ) -> RuntimeSandboxPolicy {
        RuntimeSandboxPolicy {
            writable_root: workspace_root,
            readonly_roots: Vec::new(),
            mounts: vec![readonly_host_dir_mount("selected-folder", source, target)],
            network: NetworkPolicy::Enabled,
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_mount_materialization_creates_symlink_and_adds_readonly_root() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        let target = workspace_root.join("workspace/mounts/selected-folder");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let canonical_source = source.canonicalize().unwrap();
        let mut policy =
            policy_with_mount(workspace_root, canonical_source.clone(), target.clone());

        materialize_active_mounts_with_platform_support(&mut policy, true).unwrap();

        assert_eq!(std::fs::read_link(&target).unwrap(), canonical_source);
        assert_eq!(policy.readonly_roots, vec![source.canonicalize().unwrap()]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_mount_materialization_accepts_existing_correct_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        let target = workspace_root.join("workspace/mounts/selected-folder");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let canonical_source = source.canonicalize().unwrap();
        symlink(&canonical_source, &target).unwrap();
        let mut policy =
            policy_with_mount(workspace_root, canonical_source.clone(), target.clone());

        materialize_active_mounts_with_platform_support(&mut policy, true).unwrap();

        assert_eq!(std::fs::read_link(&target).unwrap(), canonical_source);
        assert_eq!(policy.readonly_roots, vec![source.canonicalize().unwrap()]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_mount_materialization_rejects_existing_conflicting_path() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        let target = workspace_root.join("workspace/mounts/selected-folder");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "conflict").unwrap();
        let canonical_source = source.canonicalize().unwrap();
        let mut policy = policy_with_mount(workspace_root, canonical_source, target);

        let err = materialize_active_mounts_with_platform_support(&mut policy, true).unwrap_err();

        assert_eq!(err.code, "MOUNT_TARGET_CONFLICT");
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_mount_materialization_rejects_symlinked_workspace_without_creating_outside_mounts() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        let outside = temp.path().join("outside");
        let target = workspace_root.join("workspace/mounts/selected-folder");
        std::fs::create_dir_all(&workspace_root).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, workspace_root.join("workspace")).unwrap();
        let canonical_source = source.canonicalize().unwrap();
        let mut policy = policy_with_mount(workspace_root, canonical_source, target);

        let err = materialize_active_mounts_with_platform_support(&mut policy, true).unwrap_err();

        assert_eq!(err.code, "MOUNT_TARGET_INVALID");
        assert!(!outside.join("mounts").exists());
    }

    #[test]
    fn sandbox_mount_materialization_unsupported_platform_active_mounts_fail() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        let target = workspace_root.join("workspace/mounts/selected-folder");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let canonical_source = source.canonicalize().unwrap();
        let mut policy = policy_with_mount(workspace_root, canonical_source, target);

        let err = materialize_active_mounts_with_platform_support(&mut policy, false).unwrap_err();

        assert_eq!(err.code, "MOUNT_PLATFORM_UNSUPPORTED");
    }
}
