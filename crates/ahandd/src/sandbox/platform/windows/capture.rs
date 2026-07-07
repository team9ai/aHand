use std::path::Path;
use std::time::Duration;

use crate::sandbox::runner::{PlatformExecuteRequest, RuntimeSandboxPolicy};
use crate::sandbox::types::{RuntimeExecuteResult, SandboxError, SandboxResult};

pub(super) fn run_capture(
    request: PlatformExecuteRequest,
    timeout: Duration,
) -> SandboxResult<RuntimeExecuteResult> {
    let env = super::env::normalize_env(request.env, request.policy.network)?;
    let network_mode = super::network::mode_for_policy(request.policy.network)?;
    let network_context =
        super::setup::prepare_network_context(network_mode, &env, &request.sandbox_state_root)?;
    let sandbox_creds = network_context.sandbox_creds.as_ref().ok_or_else(|| {
        SandboxError::unavailable("Windows sandbox setup did not return sandbox user credentials")
    })?;
    let mut roots = filesystem_roots_for_security(&request.policy, &request.sandbox_state_root);
    add_runner_read_roots(&mut roots);
    let capability =
        super::cap::capability_for_root(&request.policy.writable_root).map_err(|err| {
            SandboxError::unavailable(format!(
                "failed to prepare Windows sandbox capability SID: {err}"
            ))
        })?;
    let mut sandbox_group_sid =
        super::sandbox_users::resolve_sandbox_users_group_sid().map_err(|err| {
            SandboxError::unavailable(format!(
                "failed to resolve Windows sandbox users group SID: {err}"
            ))
        })?;
    let mut capability_sid = super::winutil::sid_bytes_from_string(capability.sid_string())
        .map_err(|err| {
            SandboxError::unavailable(format!(
                "failed to convert Windows sandbox capability SID: {err}"
            ))
        })?;
    let sandbox_group_sid_ptr = sandbox_group_sid.as_mut_ptr() as *mut std::ffi::c_void;
    let capability_sid_ptr = capability_sid.as_mut_ptr() as *mut std::ffi::c_void;
    super::acl::apply_filesystem_roots(&roots, sandbox_group_sid_ptr, capability_sid_ptr).map_err(
        |err| {
            SandboxError::unavailable(format!(
                "failed to apply Windows sandbox filesystem ACLs: {err}"
            ))
        },
    )?;
    super::acl::allow_null_device(capability_sid_ptr).map_err(|err| {
        SandboxError::unavailable(format!(
            "failed to allow Windows NUL device for sandbox: {err}"
        ))
    })?;

    super::runner_ipc::spawn_capture(
        sandbox_creds,
        super::runner_ipc::RunnerRequest {
            command: request.command,
            cwd: request.cwd,
            env,
            timeout_ms: timeout.as_millis().min(u128::from(u64::MAX)) as u64,
            capability_sid: capability.sid_string().to_string(),
        },
    )
}

#[cfg_attr(not(test), allow(dead_code))]
fn filesystem_roots_for_security(
    policy: &RuntimeSandboxPolicy,
    state_root: &Path,
) -> super::roots::DerivedFilesystemRoots {
    super::roots::derive_filesystem_roots(policy, state_root)
}

fn add_runner_read_roots(roots: &mut super::roots::DerivedFilesystemRoots) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(parent) = exe.parent() else {
        return;
    };
    let root = parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf());
    if !roots.write_roots.iter().any(|existing| existing == &root)
        && !roots.read_roots.iter().any(|existing| existing == &root)
    {
        roots.read_roots.push(root);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use crate::sandbox::runner::{PlatformExecuteRequest, RuntimeSandboxPolicy};
    use crate::sandbox::types::NetworkPolicy;

    use super::*;

    #[test]
    fn enabled_network_reaches_setup_instead_of_static_filesystem_gate() {
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
                writable_roots: Vec::new(),
                readonly_roots: vec![],
                mounts: Vec::new(),
                network: NetworkPolicy::Enabled,
            },
            sandbox_state_root: state_root.clone(),
        };

        let err = run_capture(request, Duration::from_secs(1)).unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
        assert!(err.message.contains("sandbox user setup"));
        assert!(!workspace.join(".ahand-sandbox").join("cap_sid").exists());
        assert!(!workspace.join(".ahand-sandbox").exists());
    }

    #[test]
    fn disabled_network_reaches_offline_setup_instead_of_static_runner_gate() {
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
                writable_roots: Vec::new(),
                readonly_roots: vec![],
                mounts: Vec::new(),
                network: NetworkPolicy::Disabled,
            },
            sandbox_state_root: state_root.clone(),
        };

        let err = run_capture(request, Duration::from_secs(1)).unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
        assert!(err.message.contains("hard network blocking"));
        assert!(!workspace.join(".ahand-sandbox").exists());
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
                writable_roots: Vec::new(),
                readonly_roots: vec![runtime.clone(), sandbox_dir, secrets_dir],
                mounts: Vec::new(),
                network: NetworkPolicy::Enabled,
            },
            &state_root,
        );

        assert_eq!(roots.write_roots, vec![workspace.canonicalize().unwrap()]);
        assert_eq!(roots.read_roots, vec![runtime.canonicalize().unwrap()]);
    }

    #[test]
    fn runner_read_roots_include_current_executable_parent() {
        let mut roots = super::super::roots::DerivedFilesystemRoots {
            write_roots: Vec::new(),
            read_roots: Vec::new(),
        };

        add_runner_read_roots(&mut roots);

        assert!(!roots.read_roots.is_empty());
    }
}
