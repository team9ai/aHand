use crate::sandbox::runner::PlatformExecuteRequest;
use crate::sandbox::types::{RuntimeExecuteResult, SandboxError, SandboxResult};

mod acl;
mod cap;
mod capture;
mod env;
mod network;
mod path;
mod process;
mod setup;
mod setup_error;
mod token;

pub async fn execute(request: PlatformExecuteRequest) -> SandboxResult<RuntimeExecuteResult> {
    let timeout = request.timeout;
    tokio::task::spawn_blocking(move || capture::run_capture(request, timeout))
        .await
        .map_err(|err| {
            SandboxError::unavailable(format!("Windows sandbox worker failed to join: {err}"))
        })?
}
