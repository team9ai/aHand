use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use ahandd::sandbox::{
    NetworkPolicy,
    runner::{self, PlatformExecuteRequest, RuntimeSandboxPolicy},
};

#[tokio::test]
async fn proxy_only_is_unsupported_for_sandbox_execution() {
    let temp = tempfile::tempdir().unwrap();

    let err = runner::execute(PlatformExecuteRequest {
        executable: PathBuf::from("ignored"),
        args: vec![],
        cwd: temp.path().to_path_buf(),
        env: HashMap::new(),
        timeout: Duration::from_secs(1),
        policy: RuntimeSandboxPolicy {
            writable_root: temp.path().to_path_buf(),
            readonly_roots: vec![],
            network: NetworkPolicy::ProxyOnly,
        },
        sandbox_state_root: temp.path().join("windows-sandbox"),
    })
    .await
    .unwrap_err();

    assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
    assert!(err.message.contains("ProxyOnly"));
}
