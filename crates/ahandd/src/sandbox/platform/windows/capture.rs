use std::fs;
use std::path::{Path, PathBuf};
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
    let _stub_capability_cleanup =
        match StubCapabilityCleanup::for_root(&request.policy.writable_root) {
            Ok(cleanup) => cleanup,
            Err(err) => {
                return Err(SandboxError::unavailable(format!(
                    "failed to prepare Windows sandbox capability SID for '{}': {err}",
                    request.policy.writable_root.display()
                )));
            }
        };
    let capability =
        super::cap::capability_for_root(&request.policy.writable_root).map_err(|err| {
            SandboxError::unavailable(format!(
                "failed to prepare Windows sandbox capability SID for '{}': {err}",
                request.policy.writable_root.display()
            ))
        })?;
    let capability_sid = capability.sid_string().to_string();
    let executable = super::path::absolute(&request.executable).map_err(|err| {
        SandboxError::unavailable(format!(
            "failed to resolve Windows sandbox executable '{}': {err}",
            request.executable.display()
        ))
    })?;
    let cwd = super::path::absolute(&request.cwd).map_err(|err| {
        SandboxError::unavailable(format!(
            "failed to resolve Windows sandbox cwd '{}': {err}",
            request.cwd.display()
        ))
    })?;
    let executable_wide = super::path::wide_null(&executable);
    let cwd_wide = super::path::wide_null(&cwd);
    let null_device_wide = super::path::string_wide_null("NUL");
    let security_preparation = prepare_security_for_process;
    let _ = (
        env,
        executable_wide,
        cwd_wide,
        null_device_wide,
        capability_sid,
        security_preparation,
        network_context,
        request.args,
        request.policy,
        timeout,
    );
    Err(SandboxError::unavailable(
        "Windows restricted runtime execution requires the aHand Windows sandbox backend",
    ))
}

#[allow(dead_code)]
struct PreparedWindowsSecurity {
    token: super::token::RestrictedToken,
    applied_acl: Vec<super::acl::AppliedAcl>,
}

#[cfg_attr(not(windows), allow(dead_code))]
fn prepare_security_for_process(
    capability: &super::cap::CapabilitySid,
    policy: &RuntimeSandboxPolicy,
) -> SandboxResult<PreparedWindowsSecurity> {
    let token = super::token::create(capability).map_err(|err| {
        SandboxError::unavailable(format!("failed to create Windows sandbox token: {err}"))
    })?;
    let applied_acl = super::acl::apply_policy(
        &policy.writable_root,
        &policy.readonly_roots,
        token.capability_sid(),
    )
    .map_err(|err| {
        SandboxError::unavailable(format!("failed to apply Windows sandbox ACLs: {err}"))
    })?;
    super::acl::allow_null_device(token.capability_sid()).map_err(|err| {
        SandboxError::unavailable(format!(
            "failed to allow Windows sandbox access to NUL: {err}"
        ))
    })?;

    Ok(PreparedWindowsSecurity { token, applied_acl })
}

struct StubCapabilityCleanup {
    cap_file: PathBuf,
    cap_dir: PathBuf,
    cap_file_existed: bool,
    cap_dir_existed: bool,
}

impl StubCapabilityCleanup {
    fn for_root(root: &Path) -> std::io::Result<Self> {
        let canonical_root = root.canonicalize()?;
        let cap_dir = canonical_root.join(".ahand-sandbox");
        let cap_file = cap_dir.join("cap_sid");

        Ok(Self {
            cap_file_existed: cap_file.exists(),
            cap_dir_existed: cap_dir.exists(),
            cap_file,
            cap_dir,
        })
    }
}

impl Drop for StubCapabilityCleanup {
    fn drop(&mut self) {
        if !self.cap_file_existed {
            let _ = fs::remove_file(&self.cap_file);
        }

        if !self.cap_dir_existed
            && fs::read_dir(&self.cap_dir)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false)
        {
            let _ = fs::remove_dir(&self.cap_dir);
        }
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

    #[test]
    fn unavailable_stub_does_not_persist_new_capability_sid() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let request = PlatformExecuteRequest {
            executable: PathBuf::from("tool.exe"),
            args: vec![],
            cwd: workspace.clone(),
            env: HashMap::new(),
            timeout: Duration::from_secs(1),
            policy: RuntimeSandboxPolicy {
                writable_root: workspace.clone(),
                readonly_roots: vec![],
                network: NetworkPolicy::Enabled,
            },
            sandbox_state_root: temp.path().join("windows-sandbox"),
        };

        let err = run_capture(request, Duration::from_secs(1)).unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
        assert_eq!(
            err.message,
            "Windows restricted runtime execution requires the aHand Windows sandbox backend"
        );
        assert!(!workspace.join(".ahand-sandbox").join("cap_sid").exists());
        assert!(!workspace.join(".ahand-sandbox").exists());
    }
}
