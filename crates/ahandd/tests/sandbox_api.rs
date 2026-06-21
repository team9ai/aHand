use std::time::Duration;

use ahandd::{
    DaemonConfig,
    sandbox::{NetworkPolicy, SandboxPermissionMode, SandboxSessionConfig},
};

#[tokio::test]
async fn daemon_handle_exposes_sandbox_permission_updates() {
    let temp = tempfile::tempdir().unwrap();
    let identity_dir = temp.path().join("identity");
    let sandbox_root = temp.path().join("sandbox");

    let cfg = DaemonConfig::builder("ws://127.0.0.1:9/ws", "test-token", &identity_dir)
        .heartbeat_interval(Duration::from_millis(50))
        .build();
    let handle = ahandd::spawn(cfg).await.unwrap();

    handle
        .create_sandbox_session(SandboxSessionConfig {
            session_id: "session-1".to_string(),
            permission_mode: SandboxPermissionMode::Readonly,
            workspace_root: sandbox_root,
            network: NetworkPolicy::Enabled,
        })
        .await
        .unwrap();
    let snapshot = handle
        .update_sandbox_permission_mode("session-1", SandboxPermissionMode::Full)
        .await
        .unwrap();

    assert_eq!(snapshot.mode, SandboxPermissionMode::Full);
    assert_eq!(snapshot.version, 2);

    handle.shutdown().await.unwrap();
}
