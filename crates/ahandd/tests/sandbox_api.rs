#[cfg(target_os = "macos")]
use std::collections::HashMap;
use std::{path::PathBuf, time::Duration};

#[cfg(target_os = "macos")]
use ahandd::sandbox::{RuntimeExecuteRequest, RuntimeProviderConfig};
use ahandd::{
    AppToolDef, AppToolHandler, DaemonConfig, args_only_handler,
    sandbox::{
        HostFileRef, MountAccess, MountScope, MountSource, NetworkPolicy, RegisterVersionRequest,
        SandboxExecRequest, SandboxInvocationContext, SandboxMountSpec, SandboxPermissionMode,
        SandboxSessionConfig,
    },
};

#[tokio::test]
async fn daemon_handle_exposes_sandbox_permission_updates() {
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
            mounts: Vec::new(),
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
async fn sandbox_api_daemon_handle_registers_lists_and_unregisters_sandbox_mounts() {
    let temp = tempfile::tempdir().unwrap();
    let identity_dir = temp.path().join("identity");
    let sandbox_root = temp.path().join("sandbox");
    let source = temp.path().join("host");
    std::fs::create_dir_all(&sandbox_root).unwrap();
    std::fs::create_dir_all(&source).unwrap();

    let cfg = DaemonConfig::builder("ws://127.0.0.1:9/ws", "test-token", &identity_dir)
        .heartbeat_interval(Duration::from_millis(50))
        .build();
    let handle = ahandd::spawn(cfg).await.unwrap();
    handle
        .create_sandbox_session(SandboxSessionConfig {
            session_id: "session-1".to_string(),
            permission_mode: SandboxPermissionMode::Readonly,
            workspace_root: sandbox_root.clone(),
            network: NetworkPolicy::Enabled,
            mounts: Vec::new(),
        })
        .await
        .unwrap();

    let registered = handle
        .register_sandbox_mount(
            "session-1",
            SandboxMountSpec {
                mount_id: "selected-folder".to_string(),
                source: MountSource::HostPath(source.clone()),
                access: MountAccess::ReadOnly,
                scope: MountScope::Run {
                    run_id: "run-1".to_string(),
                },
                target: None,
                env_var: Some("COFFICE_SELECTED_FOLDER_DIR".to_string()),
            },
        )
        .await
        .unwrap();
    let mounts = handle.list_sandbox_mounts("session-1").await.unwrap();

    assert_eq!(mounts, vec![registered.clone()]);
    assert_eq!(
        registered.source,
        MountSource::HostPath(source.canonicalize().unwrap())
    );
    assert_eq!(
        registered.target,
        sandbox_root
            .canonicalize()
            .unwrap()
            .join("workspace/mounts/selected-folder")
    );

    handle
        .unregister_sandbox_mount(
            "session-1",
            "selected-folder",
            MountScope::Run {
                run_id: "run-1".to_string(),
            },
        )
        .await
        .unwrap();

    assert!(
        handle
            .list_sandbox_mounts("session-1")
            .await
            .unwrap()
            .is_empty()
    );

    handle.shutdown().await.unwrap();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn sandbox_api_execute_sandbox_command_request_env_cannot_override_active_mount_env() {
    let temp = tempfile::tempdir().unwrap();
    let identity_dir = temp.path().join("identity");
    let sandbox_root = temp.path().join("sandbox");
    let source = temp.path().join("host");
    std::fs::create_dir_all(&sandbox_root).unwrap();
    std::fs::create_dir_all(&source).unwrap();

    let cfg = DaemonConfig::builder("ws://127.0.0.1:9/ws", "test-token", &identity_dir)
        .heartbeat_interval(Duration::from_millis(50))
        .build();
    let handle = ahandd::spawn(cfg).await.unwrap();
    handle
        .create_sandbox_session(SandboxSessionConfig {
            session_id: "session-1".to_string(),
            permission_mode: SandboxPermissionMode::Readonly,
            workspace_root: sandbox_root.clone(),
            network: NetworkPolicy::Enabled,
            mounts: Vec::new(),
        })
        .await
        .unwrap();
    handle
        .register_sandbox_runtime(
            "session-1",
            RuntimeProviderConfig {
                name: "shell".to_string(),
                executable: PathBuf::from("/bin/sh"),
                readonly_roots: vec![PathBuf::from("/bin")],
                env: HashMap::new(),
                default_timeout: Duration::from_secs(3),
            },
        )
        .await
        .unwrap();
    let registered = handle
        .register_sandbox_mount(
            "session-1",
            SandboxMountSpec {
                mount_id: "selected-folder".to_string(),
                source: MountSource::HostPath(source),
                access: MountAccess::ReadOnly,
                scope: MountScope::Run {
                    run_id: "run-1".to_string(),
                },
                target: None,
                env_var: Some("COFFICE_SELECTED_FOLDER_DIR".to_string()),
            },
        )
        .await
        .unwrap();

    let result = handle
        .execute_sandbox_command(
            "session-1",
            SandboxExecRequest {
                command: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "printf '%s' \"$COFFICE_SELECTED_FOLDER_DIR\"".to_string(),
                ],
                cwd: None,
                env: HashMap::from([(
                    "COFFICE_SELECTED_FOLDER_DIR".to_string(),
                    "/tmp/spoofed".to_string(),
                )]),
                timeout: Some(Duration::from_secs(3)),
                context: Some(SandboxInvocationContext {
                    run_id: Some("run-1".to_string()),
                    invocation_id: None,
                }),
            },
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout, registered.target.to_string_lossy());

    handle.shutdown().await.unwrap();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn sandbox_api_execute_sandbox_command_request_env_for_inactive_mount_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let identity_dir = temp.path().join("identity");
    let sandbox_root = temp.path().join("sandbox");
    let source = temp.path().join("host");
    std::fs::create_dir_all(&sandbox_root).unwrap();
    std::fs::create_dir_all(&source).unwrap();

    let cfg = DaemonConfig::builder("ws://127.0.0.1:9/ws", "test-token", &identity_dir)
        .heartbeat_interval(Duration::from_millis(50))
        .build();
    let handle = ahandd::spawn(cfg).await.unwrap();
    handle
        .create_sandbox_session(SandboxSessionConfig {
            session_id: "session-1".to_string(),
            permission_mode: SandboxPermissionMode::Readonly,
            workspace_root: sandbox_root.clone(),
            network: NetworkPolicy::Enabled,
            mounts: Vec::new(),
        })
        .await
        .unwrap();
    handle
        .register_sandbox_runtime(
            "session-1",
            RuntimeProviderConfig {
                name: "shell".to_string(),
                executable: PathBuf::from("/bin/sh"),
                readonly_roots: vec![PathBuf::from("/bin")],
                env: HashMap::new(),
                default_timeout: Duration::from_secs(3),
            },
        )
        .await
        .unwrap();
    handle
        .register_sandbox_mount(
            "session-1",
            SandboxMountSpec {
                mount_id: "selected-folder".to_string(),
                source: MountSource::HostPath(source),
                access: MountAccess::ReadOnly,
                scope: MountScope::Run {
                    run_id: "run-1".to_string(),
                },
                target: None,
                env_var: Some("COFFICE_SELECTED_FOLDER_DIR".to_string()),
            },
        )
        .await
        .unwrap();

    let err = handle
        .execute_sandbox_command(
            "session-1",
            SandboxExecRequest {
                command: vec!["sh".to_string(), "-c".to_string(), "true".to_string()],
                cwd: None,
                env: HashMap::from([(
                    "COFFICE_SELECTED_FOLDER_DIR".to_string(),
                    "/tmp/spoofed".to_string(),
                )]),
                timeout: Some(Duration::from_secs(3)),
                context: Some(SandboxInvocationContext {
                    run_id: Some("run-2".to_string()),
                    invocation_id: None,
                }),
            },
        )
        .await
        .unwrap_err();

    assert_eq!(err.code, "MOUNT_SCOPE_MISMATCH");

    handle.shutdown().await.unwrap();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn sandbox_api_execute_sandbox_command_inactive_mount_without_request_env_succeeds() {
    let temp = tempfile::tempdir().unwrap();
    let identity_dir = temp.path().join("identity");
    let sandbox_root = temp.path().join("sandbox");
    let source = temp.path().join("host");
    std::fs::create_dir_all(&sandbox_root).unwrap();
    std::fs::create_dir_all(&source).unwrap();

    let cfg = DaemonConfig::builder("ws://127.0.0.1:9/ws", "test-token", &identity_dir)
        .heartbeat_interval(Duration::from_millis(50))
        .build();
    let handle = ahandd::spawn(cfg).await.unwrap();
    handle
        .create_sandbox_session(SandboxSessionConfig {
            session_id: "session-1".to_string(),
            permission_mode: SandboxPermissionMode::Readonly,
            workspace_root: sandbox_root.clone(),
            network: NetworkPolicy::Enabled,
            mounts: Vec::new(),
        })
        .await
        .unwrap();
    handle
        .register_sandbox_runtime(
            "session-1",
            RuntimeProviderConfig {
                name: "shell".to_string(),
                executable: PathBuf::from("/bin/sh"),
                readonly_roots: vec![PathBuf::from("/bin")],
                env: HashMap::new(),
                default_timeout: Duration::from_secs(3),
            },
        )
        .await
        .unwrap();
    handle
        .register_sandbox_mount(
            "session-1",
            SandboxMountSpec {
                mount_id: "selected-folder".to_string(),
                source: MountSource::HostPath(source),
                access: MountAccess::ReadOnly,
                scope: MountScope::Run {
                    run_id: "run-1".to_string(),
                },
                target: None,
                env_var: Some("COFFICE_SELECTED_FOLDER_DIR".to_string()),
            },
        )
        .await
        .unwrap();

    let result = handle
        .execute_sandbox_command(
            "session-1",
            SandboxExecRequest {
                command: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "test -z \"${COFFICE_SELECTED_FOLDER_DIR:-}\"".to_string(),
                ],
                cwd: None,
                env: HashMap::new(),
                timeout: Some(Duration::from_secs(3)),
                context: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(result.exit_code, Some(0));

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
    let handler: AppToolHandler =
        args_only_handler(|args| async move { Ok(serde_json::json!({ "received": args })) });

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

    assert!(err.to_string().contains("invalid tool name"));

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

#[tokio::test]
async fn daemon_handle_persists_and_user_commits_candidate_versions() {
    let temp = tempfile::tempdir().unwrap();
    let identity_dir = temp.path().join("identity");
    let sandbox_root = temp.path().join("sandbox");
    let source = temp.path().join("source.txt");
    std::fs::create_dir_all(sandbox_root.join("workspace")).unwrap();
    std::fs::write(&source, "original").unwrap();

    let cfg = DaemonConfig::builder("ws://127.0.0.1:9/ws", "test-token", &identity_dir)
        .heartbeat_interval(Duration::from_millis(50))
        .build();
    let handle = ahandd::spawn(cfg).await.unwrap();
    handle
        .create_sandbox_session(SandboxSessionConfig {
            session_id: "session-1".to_string(),
            permission_mode: SandboxPermissionMode::Readonly,
            workspace_root: sandbox_root.clone(),
            network: NetworkPolicy::Enabled,
            mounts: Vec::new(),
        })
        .await
        .unwrap();
    handle
        .import_sandbox_file(
            "session-1",
            HostFileRef {
                file_ref_id: "file-ref-1".to_string(),
                source_path: source.clone(),
                display_name: "source.txt".to_string(),
                size: 8,
                mtime_ms: None,
                conversation_id: None,
            },
        )
        .await
        .unwrap();
    std::fs::write(sandbox_root.join("workspace/out.txt"), "updated").unwrap();

    let version = handle
        .register_sandbox_file_version(
            "session-1",
            RegisterVersionRequest {
                sandbox_path: PathBuf::from("workspace/out.txt"),
                source_file_ref_id: Some("file-ref-1".to_string()),
            },
        )
        .await
        .unwrap();
    let versions = handle
        .list_sandbox_file_versions("session-1")
        .await
        .unwrap();
    let agent_err = handle
        .commit_sandbox_file_version("session-1", &version.version_id)
        .await
        .unwrap_err();

    assert_eq!(versions, vec![version.clone()]);
    assert_eq!(agent_err.code, "PERMISSION_DENIED");

    let result = handle
        .confirm_sandbox_file_version_overwrite("session-1", &version.version_id)
        .await
        .unwrap();

    assert_eq!(std::fs::read_to_string(&source).unwrap(), "updated");
    assert_eq!(result.version_id, version.version_id);
    assert_eq!(result.source_file_ref_id, "file-ref-1");
    assert!(result.backup_id.is_some());
    assert_eq!(result.bytes_written, 7);
    assert_eq!(result.permission_mode, SandboxPermissionMode::Readonly);

    let versions = handle
        .list_sandbox_file_versions("session-1")
        .await
        .unwrap();
    assert_eq!(
        versions[0].status,
        ahandd::sandbox::FileVersionStatus::Committed
    );

    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn daemon_handle_saves_candidate_version_as_user_selected_file() {
    let temp = tempfile::tempdir().unwrap();
    let identity_dir = temp.path().join("identity");
    let sandbox_root = temp.path().join("sandbox");
    let target = temp.path().join("exports").join("out.txt");
    std::fs::create_dir_all(sandbox_root.join("workspace")).unwrap();
    std::fs::write(sandbox_root.join("workspace/out.txt"), "copy").unwrap();

    let cfg = DaemonConfig::builder("ws://127.0.0.1:9/ws", "test-token", &identity_dir)
        .heartbeat_interval(Duration::from_millis(50))
        .build();
    let handle = ahandd::spawn(cfg).await.unwrap();
    handle
        .create_sandbox_session(SandboxSessionConfig {
            session_id: "session-1".to_string(),
            permission_mode: SandboxPermissionMode::Copy,
            workspace_root: sandbox_root,
            network: NetworkPolicy::Enabled,
            mounts: Vec::new(),
        })
        .await
        .unwrap();
    let version = handle
        .register_sandbox_file_version(
            "session-1",
            RegisterVersionRequest {
                sandbox_path: PathBuf::from("workspace/out.txt"),
                source_file_ref_id: None,
            },
        )
        .await
        .unwrap();

    let result = handle
        .save_sandbox_file_version_as("session-1", &version.version_id, &target)
        .await
        .unwrap();

    assert_eq!(std::fs::read_to_string(&target).unwrap(), "copy");
    assert_eq!(result.version_id, version.version_id);
    assert_eq!(result.source_file_ref_id, target.to_string_lossy());
    assert_eq!(result.backup_id, None);
    assert_eq!(result.old_hash, None);
    assert_eq!(result.bytes_written, 4);
    assert_eq!(result.permission_mode, SandboxPermissionMode::Copy);

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
            mounts: Vec::new(),
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
