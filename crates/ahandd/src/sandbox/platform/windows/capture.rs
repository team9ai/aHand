use std::path::Path;
use std::time::Duration;

use crate::sandbox::runner::{PlatformExecuteRequest, RuntimeSandboxPolicy};
use crate::sandbox::types::{RuntimeExecuteResult, SandboxError, SandboxResult};

pub(super) fn run_capture(
    request: PlatformExecuteRequest,
    _timeout: Duration,
) -> SandboxResult<RuntimeExecuteResult> {
    let env = super::env::normalize_env(request.env, request.policy.network)?;
    let network_mode = super::network::mode_for_policy(request.policy.network)?;
    if !process_execution_enabled() {
        return match network_mode {
            super::network::WindowsNetworkMode::Offline => Err(SandboxError::unavailable(
                "NetworkPolicy::Disabled requires the Windows offline sandbox user runner before execution can be enabled",
            )),
            super::network::WindowsNetworkMode::Online => Err(SandboxError::unavailable(
                "Windows sandbox execution requires filesystem default-deny isolation before process launch can be enabled",
            )),
        };
    }
    let network_context =
        super::setup::prepare_network_context(network_mode, &env, &request.sandbox_state_root)?;
    let runner_launch = resolve_sandbox_user_runner_launch(&network_context)?;

    match runner_launch {}
}

fn process_execution_enabled() -> bool {
    false
}

#[derive(Debug)]
enum SandboxUserRunnerLaunch {}

fn resolve_sandbox_user_runner_launch(
    _network_context: &super::setup::WindowsNetworkContext,
) -> SandboxResult<SandboxUserRunnerLaunch> {
    Err(SandboxError::unavailable(
        "Windows sandbox execution requires sandbox user runner/logon integration with CreateProcessWithLogonW before process launch can be enabled",
    ))
}

#[cfg_attr(not(test), allow(dead_code))]
fn filesystem_roots_for_security(
    policy: &RuntimeSandboxPolicy,
    state_root: &Path,
) -> super::roots::DerivedFilesystemRoots {
    super::roots::derive_filesystem_roots(policy, state_root)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;

    use crate::sandbox::runner::{PlatformExecuteRequest, RuntimeSandboxPolicy};
    use crate::sandbox::types::NetworkPolicy;

    use super::*;

    #[test]
    fn enabled_network_fails_before_capability_sid_creation_until_filesystem_isolation_exists() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let state_root = temp.path().join("windows-sandbox");
        std::fs::create_dir_all(&workspace).unwrap();
        let request = PlatformExecuteRequest {
            command: vec!["tool.exe".to_string()],
            cwd: workspace.clone(),
            env: HashMap::new(),
            timeout: Duration::from_secs(1),
            policy: RuntimeSandboxPolicy {
                writable_root: workspace.clone(),
                readonly_roots: vec![],
                network: NetworkPolicy::Enabled,
            },
            sandbox_state_root: state_root.clone(),
        };

        let err = run_capture(request, Duration::from_secs(1)).unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
        assert!(err.message.contains("filesystem default-deny"));
        assert!(!workspace.join(".ahand-sandbox").join("cap_sid").exists());
        assert!(!workspace.join(".ahand-sandbox").exists());
        assert!(!super::super::setup::sandbox_dir(&state_root).exists());
    }

    #[test]
    fn disabled_network_fails_before_capability_sid_creation_until_runner_exists() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let state_root = temp.path().join("windows-sandbox");
        std::fs::create_dir_all(&workspace).unwrap();
        let request = PlatformExecuteRequest {
            command: vec!["tool.exe".to_string()],
            cwd: workspace.clone(),
            env: HashMap::new(),
            timeout: Duration::from_secs(1),
            policy: RuntimeSandboxPolicy {
                writable_root: workspace.clone(),
                readonly_roots: vec![],
                network: NetworkPolicy::Disabled,
            },
            sandbox_state_root: state_root.clone(),
        };

        let err = run_capture(request, Duration::from_secs(1)).unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
        assert!(err.message.contains("offline sandbox user runner"));
        assert!(!workspace.join(".ahand-sandbox").exists());
        assert!(!super::super::setup::sandbox_dir(&state_root).exists());
    }

    #[test]
    fn security_filesystem_roots_filter_sandbox_state_before_acl_setup() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let runtime = temp.path().join("runtime");
        let state_root = temp.path().join("windows-sandbox");
        let sandbox_dir = super::super::setup::sandbox_dir(&state_root);
        let secrets_dir = super::super::setup::sandbox_secrets_dir(&state_root);
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::create_dir_all(&sandbox_dir).unwrap();
        std::fs::create_dir_all(&secrets_dir).unwrap();

        let roots = filesystem_roots_for_security(
            &RuntimeSandboxPolicy {
                writable_root: workspace.clone(),
                readonly_roots: vec![runtime.clone(), sandbox_dir, secrets_dir],
                network: NetworkPolicy::Enabled,
            },
            &state_root,
        );

        assert_eq!(roots.write_roots, vec![workspace.canonicalize().unwrap()]);
        assert_eq!(roots.read_roots, vec![runtime.canonicalize().unwrap()]);
    }

    #[test]
    fn sandbox_user_runner_launch_is_unavailable_until_logon_runner_exists() {
        let context = super::super::setup::WindowsNetworkContext {
            mode: super::super::network::WindowsNetworkMode::Online,
            state_root: PathBuf::from("state"),
            sandbox_creds: Some(super::super::identity::SandboxCreds {
                username: super::super::setup::ONLINE_USERNAME.to_string(),
                password: "password".to_string(),
            }),
        };

        let err = resolve_sandbox_user_runner_launch(&context).unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
        assert!(err.message.contains("sandbox user runner/logon"));
        assert!(err.message.contains("CreateProcessWithLogonW"));
    }
}
