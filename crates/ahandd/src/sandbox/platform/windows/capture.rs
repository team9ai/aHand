use std::time::Duration;

use crate::sandbox::runner::PlatformExecuteRequest;
use crate::sandbox::types::{RuntimeExecuteResult, SandboxError, SandboxResult};

pub(super) fn run_capture(
    request: PlatformExecuteRequest,
    timeout: Duration,
) -> SandboxResult<RuntimeExecuteResult> {
    let capability =
        super::cap::capability_for_root(&request.policy.writable_root).map_err(|err| {
            SandboxError::unavailable(format!(
                "failed to prepare Windows sandbox capability SID for '{}': {err}",
                request.policy.writable_root.display()
            ))
        })?;
    let capability_sid = capability.sid_string().to_string();
    let env = super::env::normalize_env(request.env, request.policy.network)?;
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
    let _ = (
        env,
        executable_wide,
        cwd_wide,
        null_device_wide,
        capability_sid,
        request.args,
        request.policy,
        timeout,
    );
    Err(SandboxError::unavailable(
        "Windows restricted runtime execution requires the aHand Windows sandbox backend",
    ))
}
