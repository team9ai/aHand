use crate::sandbox::runner::PlatformExecuteRequest;
use crate::sandbox::types::{RuntimeExecuteResult, SandboxError, SandboxResult};

pub async fn execute(_request: PlatformExecuteRequest) -> SandboxResult<RuntimeExecuteResult> {
    Err(SandboxError::unavailable(
        "aHand sandbox runtime execution is unavailable on this platform",
    ))
}
