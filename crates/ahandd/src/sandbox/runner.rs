use std::collections::HashMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use super::platform;
use super::types::{
    MountAccess, MountSource, NetworkPolicy, RegisteredSandboxMount, RuntimeExecuteResult,
    RuntimeProviderConfig, SandboxCommand, SandboxError, SandboxResult,
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
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    pub timeout: Duration,
    pub policy: RuntimeSandboxPolicy,
}

pub async fn execute(mut request: PlatformExecuteRequest) -> SandboxResult<RuntimeExecuteResult> {
    materialize_active_mounts(&mut request.policy)?;
    platform::execute(request).await
}

pub fn command_argv_from_sandbox_command(command: &SandboxCommand) -> SandboxResult<Vec<String>> {
    match command {
        SandboxCommand::Shell { cmd } => shell_argv(cmd),
        SandboxCommand::Argv { command } => {
            if command
                .first()
                .map(|program| program.trim().is_empty())
                .unwrap_or(true)
            {
                return Err(SandboxError::invalid_command(
                    "sandbox command must not be empty",
                ));
            }
            Ok(command.clone())
        }
    }
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
    let mount_namespace = canonical_workspace.join("workspace").join("mounts");
    fs::create_dir_all(&mount_namespace).map_err(|e| {
        SandboxError::mount_target_invalid(format!("failed to create mount namespace: {e}"))
    })?;
    let namespace_metadata = fs::symlink_metadata(&mount_namespace).map_err(|e| {
        SandboxError::mount_target_invalid(format!("failed to inspect mount namespace: {e}"))
    })?;
    if namespace_metadata.file_type().is_symlink() || !namespace_metadata.is_dir() {
        return Err(SandboxError::mount_target_invalid(
            "mount namespace must be a plain directory",
        ));
    }
    let canonical_namespace = mount_namespace.canonicalize().map_err(|e| {
        SandboxError::mount_target_invalid(format!("failed to resolve mount namespace: {e}"))
    })?;
    if canonical_namespace != mount_namespace {
        return Err(SandboxError::mount_target_invalid(
            "mount namespace must resolve without symlinks",
        ));
    }
    if !target.is_absolute() || contains_parent_component(target) {
        return Err(SandboxError::mount_target_invalid(
            "resolved mount target must be an absolute path without traversal",
        ));
    }
    let parent = target.parent().ok_or_else(|| {
        SandboxError::mount_target_invalid("mount target has no parent namespace")
    })?;
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
    if !canonical_target_path.starts_with(&canonical_namespace) {
        return Err(SandboxError::mount_target_invalid(
            "mount target parent escapes workspace/mounts",
        ));
    }

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

    create_symlink(canonical_source, target)
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

#[cfg(unix)]
fn shell_argv(cmd: &str) -> SandboxResult<Vec<String>> {
    if cmd.trim().is_empty() {
        return Err(SandboxError::invalid_command("cmd must not be empty"));
    }
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|value| {
            value.ends_with("/zsh") || value.ends_with("/bash") || value.ends_with("/sh")
        })
        .filter(|value| Path::new(value).exists())
        .unwrap_or_else(|| "/bin/sh".to_string());
    Ok(vec![shell, "-c".to_string(), cmd.to_string()])
}

#[cfg(windows)]
fn shell_argv(cmd: &str) -> SandboxResult<Vec<String>> {
    if cmd.trim().is_empty() {
        return Err(SandboxError::invalid_command("cmd must not be empty"));
    }
    if let Some(shell) =
        find_windows_shell("pwsh.exe").or_else(|| find_windows_shell("powershell.exe"))
    {
        return Ok(vec![
            shell,
            "-NoProfile".to_string(),
            "-Command".to_string(),
            cmd.to_string(),
        ]);
    }
    if let Some(shell) = find_windows_shell("cmd.exe") {
        return Ok(vec![shell, "/c".to_string(), cmd.to_string()]);
    }
    Err(SandboxError::command_not_found(
        "no Windows shell found for sandbox command",
    ))
}

#[cfg(windows)]
fn find_windows_shell(name: &str) -> Option<String> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    None
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

    #[cfg(unix)]
    #[test]
    fn shell_command_uses_posix_shell_c() {
        let command = command_argv_from_sandbox_command(&SandboxCommand::Shell {
            cmd: "echo ok".to_string(),
        })
        .unwrap();

        assert_eq!(command.len(), 3);
        assert_eq!(command[1], "-c");
        assert_eq!(command[2], "echo ok");
        assert!(
            command[0].ends_with("/zsh")
                || command[0].ends_with("/bash")
                || command[0].ends_with("/sh")
        );
    }

    #[test]
    fn argv_command_passes_through_without_runtime_resolution() {
        let command = command_argv_from_sandbox_command(&SandboxCommand::Argv {
            command: vec![
                "python".to_string(),
                "-c".to_string(),
                "print('ok')".to_string(),
            ],
        })
        .unwrap();

        assert_eq!(command, vec!["python", "-c", "print('ok')"]);
    }

    #[test]
    fn empty_argv_command_is_invalid() {
        let err = command_argv_from_sandbox_command(&SandboxCommand::Argv { command: vec![] })
            .unwrap_err();

        assert_eq!(err.code, "INVALID_COMMAND");
    }

    #[test]
    fn blank_shell_command_is_invalid() {
        let err = command_argv_from_sandbox_command(&SandboxCommand::Shell {
            cmd: "   ".to_string(),
        })
        .unwrap_err();

        assert_eq!(err.code, "INVALID_COMMAND");
    }

    #[tokio::test]
    async fn unsupported_platform_fails_closed() {
        let request = PlatformExecuteRequest {
            command: vec!["/bin/echo".into(), "hello".into()],
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
