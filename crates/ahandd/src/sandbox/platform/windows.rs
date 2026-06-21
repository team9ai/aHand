use crate::sandbox::runner::PlatformExecuteRequest;
use crate::sandbox::types::{RuntimeExecuteResult, SandboxError, SandboxResult};

pub async fn execute(_request: PlatformExecuteRequest) -> SandboxResult<RuntimeExecuteResult> {
    Err(SandboxError::unavailable(
        "Windows sandbox runtime execution is not wired yet",
    ))
}
