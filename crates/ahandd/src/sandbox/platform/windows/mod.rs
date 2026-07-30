#[cfg(windows)]
use crate::sandbox::runner::PlatformExecuteRequest;
#[cfg(windows)]
use crate::sandbox::types::{RuntimeExecuteResult, SandboxError, SandboxResult};
#[cfg(windows)]
use std::time::Duration;

#[cfg(windows)]
pub(super) const SANDBOX_TIMING_PREFIX: &str = "coffice-sandbox-timing-v1 ";

#[cfg(windows)]
pub(super) fn append_sandbox_timing(stderr: &mut String, stage: &str, duration: Duration) {
    use std::fmt::Write as _;

    let _ = writeln!(
        stderr,
        r#"{SANDBOX_TIMING_PREFIX}{{"stage":"{stage}","durationMs":{}}}"#,
        duration.as_millis()
    );
}

mod acl;
mod cap;
mod capture;
mod dpapi;
mod env;
mod firewall;
mod identity;
mod network;
mod path;
mod process;
mod roots;
#[cfg(windows)]
mod runner_ipc;
mod sandbox_users;
mod setup;
mod setup_error;
mod token;
mod winutil;

#[cfg(windows)]
pub async fn execute(request: PlatformExecuteRequest) -> SandboxResult<RuntimeExecuteResult> {
    let timeout = request.timeout;
    tokio::task::spawn_blocking(move || capture::run_capture(request, timeout))
        .await
        .map_err(|err| {
            SandboxError::unavailable(format!("Windows sandbox worker failed to join: {err}"))
        })?
}

pub fn try_run_helper_from_args() -> Result<bool, String> {
    #[cfg(windows)]
    if runner_ipc::try_run_from_args()? {
        return Ok(true);
    }
    setup::try_run_helper_from_args()
}
