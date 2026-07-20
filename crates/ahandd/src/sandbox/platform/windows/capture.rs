use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use crate::sandbox::runner::{PlatformExecuteRequest, RuntimeSandboxPolicy};
use crate::sandbox::types::{RuntimeExecuteResult, SandboxError, SandboxResult};

#[cfg(windows)]
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
    add_windows_default_read_roots(&mut roots);
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

fn add_windows_default_read_roots(roots: &mut super::roots::DerivedFilesystemRoots) {
    add_runner_read_roots(roots);
    #[cfg(windows)]
    {
        for root in windows_default_read_root_candidates() {
            push_existing_read_root(roots, root);
        }
    }
}

fn add_runner_read_roots(roots: &mut super::roots::DerivedFilesystemRoots) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(parent) = exe.parent() else {
        return;
    };
    push_existing_read_root(roots, parent.to_path_buf());
}

fn push_existing_read_root(roots: &mut super::roots::DerivedFilesystemRoots, root: PathBuf) {
    if !root.exists() {
        return;
    }
    let root = root.canonicalize().unwrap_or(root);
    if !roots.write_roots.iter().any(|existing| existing == &root)
        && !roots.read_roots.iter().any(|existing| existing == &root)
    {
        roots.read_roots.push(root);
    }
}

#[cfg(windows)]
fn windows_default_read_root_candidates() -> Vec<PathBuf> {
    windows_default_read_root_candidates_from_env(|key| std::env::var(key).ok())
}

#[cfg(windows)]
fn windows_default_read_root_candidates_from_env<F>(mut env: F) -> Vec<PathBuf>
where
    F: FnMut(&str) -> Option<String>,
{
    let mut candidates = Vec::new();
    let system_root = env("SystemRoot").unwrap_or_else(|| r"C:\Windows".to_string());
    push_candidate(&mut candidates, PathBuf::from(&system_root));
    for child in [
        "System32",
        "SysWOW64",
        "WinSxS",
        "Fonts",
        r"System32\WindowsPowerShell\v1.0",
    ] {
        push_candidate(&mut candidates, PathBuf::from(&system_root).join(child));
    }
    for key in [
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "ProgramData",
    ] {
        if let Some(path) = env(key).filter(|value| !value.trim().is_empty()) {
            push_candidate(&mut candidates, PathBuf::from(path));
        }
    }
    let system_drive = env("SystemDrive").unwrap_or_else(|| "C:".to_string());
    push_candidate(
        &mut candidates,
        PathBuf::from(format!(
            "{}\\Users\\{}",
            system_drive.trim_end_matches(['\\', '/']),
            super::setup::ONLINE_USERNAME
        )),
    );
    candidates
}

#[cfg(windows)]
fn push_candidate(candidates: &mut Vec<PathBuf>, path: PathBuf) {
    let key = path
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase();
    if !candidates.iter().any(|existing| {
        existing
            .to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .eq_ignore_ascii_case(&key)
    }) {
        candidates.push(path);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;

    use crate::sandbox::runner::{PlatformExecuteRequest, RuntimeSandboxPolicy};
    use crate::sandbox::types::NetworkPolicy;

    use super::*;

    #[cfg(windows)]
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

    #[cfg(windows)]
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

    #[cfg(windows)]
    #[test]
    fn windows_default_read_root_candidates_cover_system_runtime_and_profile_roots() {
        let candidates = windows_default_read_root_candidates_from_env(|key| match key {
            "SystemRoot" => Some(r"D:\Windows".to_string()),
            "ProgramFiles" => Some(r"D:\Program Files".to_string()),
            "ProgramFiles(x86)" => Some(r"D:\Program Files (x86)".to_string()),
            "ProgramW6432" => Some(r"D:\Program Files".to_string()),
            "ProgramData" => Some(r"D:\ProgramData".to_string()),
            "SystemDrive" => Some("D:".to_string()),
            _ => None,
        });

        assert!(candidates.contains(&PathBuf::from(r"D:\Windows")));
        assert!(candidates.contains(&PathBuf::from(r"D:\Windows\System32")));
        assert!(candidates.contains(&PathBuf::from(r"D:\Windows\SysWOW64")));
        assert!(candidates.contains(&PathBuf::from(r"D:\Windows\WinSxS")));
        assert!(candidates.contains(&PathBuf::from(r"D:\Program Files")));
        assert!(candidates.contains(&PathBuf::from(r"D:\Program Files (x86)")));
        assert!(candidates.contains(&PathBuf::from(r"D:\ProgramData")));
        assert!(candidates.contains(&PathBuf::from(r"D:\Users\AhandSandboxOnline")));
        assert_eq!(
            candidates
                .iter()
                .filter(|path| **path == PathBuf::from(r"D:\Program Files"))
                .count(),
            1
        );
    }
}
