use super::runner::PlatformExecuteRequest;
use super::types::{RuntimeExecuteResult, SandboxResult};

#[cfg(target_os = "macos")]
pub mod macos;
pub mod unsupported;
#[cfg(any(windows, test))]
pub mod windows;

pub async fn execute(request: PlatformExecuteRequest) -> SandboxResult<RuntimeExecuteResult> {
    #[cfg(target_os = "macos")]
    {
        return macos::execute(request).await;
    }
    #[cfg(windows)]
    {
        return windows::execute(request).await;
    }
    #[allow(unreachable_code)]
    unsupported::execute(request).await
}
