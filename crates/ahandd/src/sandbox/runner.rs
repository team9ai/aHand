use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use super::platform;
use super::types::{
    NetworkPolicy, RuntimeExecuteResult, RuntimeProviderConfig, SandboxError, SandboxResult,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSandboxPolicy {
    pub writable_root: PathBuf,
    pub readonly_roots: Vec<PathBuf>,
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

pub async fn execute(request: PlatformExecuteRequest) -> SandboxResult<RuntimeExecuteResult> {
    platform::execute(request).await
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
    use crate::sandbox::types::{NetworkPolicy, RuntimeProviderConfig};
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
                network: NetworkPolicy::Enabled,
            },
        };

        let err = platform::unsupported::execute(request).await.unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
    }
}
