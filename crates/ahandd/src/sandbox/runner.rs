use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use super::platform;
use super::types::{NetworkPolicy, RuntimeExecuteResult, RuntimeProviderConfig, SandboxResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSandboxPolicy {
    pub writable_root: PathBuf,
    pub readonly_roots: Vec<PathBuf>,
    pub network: NetworkPolicy,
}

impl RuntimeSandboxPolicy {
    pub fn new(
        writable_root: PathBuf,
        provider: RuntimeProviderConfig,
        network: NetworkPolicy,
    ) -> Self {
        Self {
            writable_root,
            readonly_roots: provider.readonly_roots,
            network,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformExecuteRequest {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    pub timeout: Duration,
    pub policy: RuntimeSandboxPolicy,
}

pub async fn execute(request: PlatformExecuteRequest) -> SandboxResult<RuntimeExecuteResult> {
    platform::execute(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::types::{NetworkPolicy, RuntimeProviderConfig};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn runtime_policy_contains_session_write_root_and_readonly_roots() {
        let provider = RuntimeProviderConfig {
            name: "python".into(),
            executable: PathBuf::from("/runtimes/python/bin/python"),
            readonly_roots: vec![PathBuf::from("/runtimes/python")],
            env: HashMap::new(),
            default_timeout: Duration::from_secs(30),
        };
        let policy = RuntimeSandboxPolicy::new(
            PathBuf::from("/sessions/s1"),
            provider,
            NetworkPolicy::Enabled,
        );

        assert_eq!(policy.writable_root, PathBuf::from("/sessions/s1"));
        assert_eq!(
            policy.readonly_roots,
            vec![PathBuf::from("/runtimes/python")]
        );
        assert_eq!(policy.network, NetworkPolicy::Enabled);
    }

    #[tokio::test]
    async fn unsupported_platform_fails_closed() {
        let request = PlatformExecuteRequest {
            executable: PathBuf::from("/bin/echo"),
            args: vec!["hello".into()],
            cwd: PathBuf::from("/tmp"),
            env: HashMap::new(),
            timeout: Duration::from_secs(1),
            policy: RuntimeSandboxPolicy {
                writable_root: PathBuf::from("/tmp"),
                readonly_roots: vec![],
                network: NetworkPolicy::Enabled,
            },
        };

        let err = platform::unsupported::execute(request).await.unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
    }
}
