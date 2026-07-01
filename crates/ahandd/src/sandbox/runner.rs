use std::collections::HashMap;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use super::platform;
use super::types::{
    NetworkPolicy, RuntimeExecuteResult, RuntimeProviderConfig, SandboxCommand, SandboxError,
    SandboxResult,
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
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    pub timeout: Duration,
    pub policy: RuntimeSandboxPolicy,
    pub sandbox_state_root: PathBuf,
}

pub async fn execute(request: PlatformExecuteRequest) -> SandboxResult<RuntimeExecuteResult> {
    if request.policy.network == NetworkPolicy::ProxyOnly {
        return Err(SandboxError::unavailable(
            "NetworkPolicy::ProxyOnly is not supported by the aHand sandbox yet",
        ));
    }
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
    async fn proxy_only_network_policy_is_rejected_before_platform_dispatch() {
        let request = PlatformExecuteRequest {
            command: vec!["ignored".into()],
            cwd: PathBuf::from("."),
            env: HashMap::new(),
            timeout: Duration::from_secs(1),
            policy: RuntimeSandboxPolicy {
                writable_root: PathBuf::from("."),
                readonly_roots: vec![],
                network: NetworkPolicy::ProxyOnly,
            },
            sandbox_state_root: PathBuf::from(".ahand-sandbox-state"),
        };

        let err = execute(request).await.unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
        assert!(err.message.contains("ProxyOnly"));
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
                network: NetworkPolicy::Enabled,
            },
            sandbox_state_root: PathBuf::from("/tmp/.ahand-sandbox-state"),
        };

        let err = platform::unsupported::execute(request).await.unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
    }
}
