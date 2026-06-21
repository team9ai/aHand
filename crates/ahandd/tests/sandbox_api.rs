use std::{collections::HashMap, path::PathBuf, time::Duration};

use ahandd::{
    AppToolDef, AppToolHandler, DaemonConfig,
    sandbox::{
        NetworkPolicy, RuntimeExecuteRequest, RuntimeProviderConfig, SandboxPermissionMode,
        SandboxSessionConfig,
    },
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

#[tokio::test]
async fn daemon_handle_registers_app_tool_handlers() {
    let temp = tempfile::tempdir().unwrap();
    let identity_dir = temp.path().join("identity");
    let cfg = DaemonConfig::builder("ws://127.0.0.1:9/ws", "test-token", &identity_dir)
        .heartbeat_interval(Duration::from_millis(50))
        .build();
    let handle = ahandd::spawn(cfg).await.unwrap();
    let handler: AppToolHandler = std::sync::Arc::new(|args| {
        Box::pin(async move { Ok(serde_json::json!({ "received": args })) })
    });

    handle
        .register_app_tool(
            AppToolDef {
                name: "import_file".to_string(),
                description: "Import a Coffice file pointer into the sandbox".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "fileRefId": { "type": "string" }
                    }
                }),
                requires_approval: false,
            },
            handler.clone(),
        )
        .await
        .unwrap();
    let err = handle
        .register_app_tool(
            AppToolDef {
                name: " ".to_string(),
                description: "invalid".to_string(),
                input_schema: serde_json::json!({ "type": "object" }),
                requires_approval: false,
            },
            handler,
        )
        .await
        .unwrap_err();

    assert!(err.to_string().contains("app tool name cannot be empty"));

    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn daemon_handle_exposes_approval_subscription_and_response() {
    let temp = tempfile::tempdir().unwrap();
    let identity_dir = temp.path().join("identity");
    let cfg = DaemonConfig::builder("ws://127.0.0.1:9/ws", "test-token", &identity_dir)
        .heartbeat_interval(Duration::from_millis(50))
        .build();
    let handle = ahandd::spawn(cfg).await.unwrap();
    let _subscription = handle.subscribe_approvals();

    assert!(
        !handle
            .respond_approval("missing-job", false, "not approved")
            .await
    );

    handle.shutdown().await.unwrap();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn daemon_handle_executes_registered_runtime_inside_sandbox() {
    let temp = tempfile::tempdir().unwrap();
    let identity_dir = temp.path().join("identity");
    let sandbox_root = temp.path().join("sandbox");
    std::fs::create_dir_all(&sandbox_root).unwrap();

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
    handle
        .register_sandbox_runtime(
            "session-1",
            RuntimeProviderConfig {
                name: "echo".to_string(),
                executable: PathBuf::from("/bin/echo"),
                readonly_roots: vec![PathBuf::from("/bin")],
                env: HashMap::new(),
                default_timeout: Duration::from_secs(5),
            },
        )
        .await
        .unwrap();

    let result = handle
        .execute_sandbox_runtime(
            "session-1",
            RuntimeExecuteRequest {
                runtime: "echo".to_string(),
                args: vec!["hello".to_string()],
                cwd: None,
                env: HashMap::new(),
                timeout: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout, "hello\n");
    assert_eq!(result.stderr, "");
    assert!(!result.timed_out);

    handle.shutdown().await.unwrap();
}
