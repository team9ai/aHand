use std::path::PathBuf;

use crate::sandbox::runner::{PlatformExecuteRequest, RuntimeSandboxPolicy};
use crate::sandbox::types::{RuntimeExecuteResult, SandboxError, SandboxResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsRuntimePolicy {
    pub writable_root: PathBuf,
    pub readonly_roots: Vec<PathBuf>,
}

impl WindowsRuntimePolicy {
    pub fn from_runtime_policy(policy: RuntimeSandboxPolicy) -> Self {
        Self {
            writable_root: policy.writable_root,
            readonly_roots: policy.readonly_roots,
        }
    }
}

pub async fn execute(request: PlatformExecuteRequest) -> SandboxResult<RuntimeExecuteResult> {
    let _policy = WindowsRuntimePolicy::from_runtime_policy(request.policy);
    Err(SandboxError::unavailable(
        "Windows restricted runtime execution requires the aHand Windows sandbox backend",
    ))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::sandbox::runner::RuntimeSandboxPolicy;
    use crate::sandbox::types::NetworkPolicy;
    use std::path::PathBuf;

    #[test]
    fn windows_policy_tracks_writable_root_and_readonly_roots() {
        let policy = WindowsRuntimePolicy::from_runtime_policy(RuntimeSandboxPolicy {
            writable_root: PathBuf::from(r"C:\sessions\s1"),
            readonly_roots: vec![PathBuf::from(r"C:\runtimes\python")],
            mounts: Vec::new(),
            network: NetworkPolicy::Enabled,
        });

        assert_eq!(policy.writable_root, PathBuf::from(r"C:\sessions\s1"));
        assert_eq!(
            policy.readonly_roots,
            vec![PathBuf::from(r"C:\runtimes\python")]
        );
    }
}
