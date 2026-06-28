use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use ahandd::sandbox::{
    MountAccess, MountScope, MountSource, MountSourceSnapshot, NetworkPolicy,
    RegisteredSandboxMount, SandboxExecRequest, SandboxInvocationContext, SandboxMountSpec,
    SandboxPermissionMode, SandboxSessionConfig,
};

#[test]
fn sandbox_types_preserve_mount_spec_intent() {
    let spec = SandboxMountSpec {
        mount_id: "selected-folder".to_string(),
        source: MountSource::HostPath(PathBuf::from("/host/Selected Folder")),
        access: MountAccess::ReadOnly,
        scope: MountScope::Run {
            run_id: "run-1".to_string(),
        },
        target: None,
        env_var: Some("COFFICE_SELECTED_FOLDER_DIR".to_string()),
    };

    assert_eq!(spec.mount_id, "selected-folder");
    assert_eq!(
        spec.source,
        MountSource::HostPath(PathBuf::from("/host/Selected Folder"))
    );
    assert_eq!(spec.access, MountAccess::ReadOnly);
    assert_eq!(
        spec.scope,
        MountScope::Run {
            run_id: "run-1".to_string()
        }
    );
    assert_eq!(spec.target, None);
    assert_eq!(
        spec.env_var,
        Some("COFFICE_SELECTED_FOLDER_DIR".to_string())
    );
}

#[test]
fn sandbox_types_preserve_registered_mount_resolution() {
    let registered = RegisteredSandboxMount {
        mount_id: "runtime-cache".to_string(),
        source: MountSource::RuntimePath(PathBuf::from("/runtime/cache")),
        access: MountAccess::CopyOnWrite,
        scope: MountScope::Invocation {
            invocation_id: "invoke-1".to_string(),
        },
        target: PathBuf::from("/sandbox/workspace/mounts/runtime-cache"),
        env_var: Some("RUNTIME_CACHE_DIR".to_string()),
        source_snapshot: MountSourceSnapshot {
            exists: true,
            is_dir: true,
        },
    };

    assert_eq!(registered.mount_id, "runtime-cache");
    assert_eq!(
        registered.source,
        MountSource::RuntimePath(PathBuf::from("/runtime/cache"))
    );
    assert_eq!(registered.access, MountAccess::CopyOnWrite);
    assert_eq!(
        registered.scope,
        MountScope::Invocation {
            invocation_id: "invoke-1".to_string()
        }
    );
    assert_eq!(
        registered.target,
        PathBuf::from("/sandbox/workspace/mounts/runtime-cache")
    );
    assert_eq!(registered.source_snapshot.exists, true);
    assert_eq!(registered.source_snapshot.is_dir, true);
}

#[test]
fn sandbox_types_attach_invocation_context_to_exec_request() {
    let request = SandboxExecRequest {
        command: vec![
            "python".to_string(),
            "-c".to_string(),
            "print(1)".to_string(),
        ],
        cwd: Some(PathBuf::from("workspace")),
        env: HashMap::from([("EXAMPLE".to_string(), "1".to_string())]),
        timeout: Some(Duration::from_secs(5)),
        context: Some(SandboxInvocationContext {
            session_id: "session-1".to_string(),
            run_id: Some("run-1".to_string()),
            scope_id: None,
            invocation_id: Some("invoke-1".to_string()),
        }),
    };

    assert_eq!(
        request.context.as_ref().unwrap().run_id.as_deref(),
        Some("run-1")
    );
    assert_eq!(
        request.context.as_ref().unwrap().invocation_id.as_deref(),
        Some("invoke-1")
    );
}

#[test]
fn sandbox_types_attach_initial_mounts_to_session_config() {
    let mount = SandboxMountSpec {
        mount_id: "session-assets".to_string(),
        source: MountSource::SandboxPath(PathBuf::from("workspace/assets")),
        access: MountAccess::ReadWrite,
        scope: MountScope::Session,
        target: Some(PathBuf::from("workspace/mounts/session-assets")),
        env_var: None,
    };

    let config = SandboxSessionConfig {
        session_id: "session-1".to_string(),
        permission_mode: SandboxPermissionMode::Readonly,
        workspace_root: PathBuf::from("/sandbox"),
        network: NetworkPolicy::Enabled,
        mounts: vec![mount.clone()],
    };

    assert_eq!(config.mounts, vec![mount]);
}

#[test]
fn sandbox_types_are_exported_from_public_api() {
    let _public_access: ahandd::MountAccess = MountAccess::WriteOnly;
    let _public_scope: ahandd::MountScope = MountScope::Session;
    let _public_source: ahandd::MountSource = MountSource::SandboxPath(PathBuf::from("workspace"));
    let _public_snapshot: ahandd::MountSourceSnapshot = MountSourceSnapshot {
        exists: false,
        is_dir: false,
    };
    let _public_context: ahandd::SandboxInvocationContext = SandboxInvocationContext {
        session_id: "session-1".to_string(),
        run_id: None,
        scope_id: None,
        invocation_id: Some("invoke-1".to_string()),
    };
    let _public_spec: ahandd::SandboxMountSpec = SandboxMountSpec {
        mount_id: "public-spec".to_string(),
        source: MountSource::HostPath(PathBuf::from("/host/public")),
        access: MountAccess::ReadOnly,
        scope: MountScope::Session,
        target: None,
        env_var: None,
    };
    let _public_mount: ahandd::RegisteredSandboxMount = RegisteredSandboxMount {
        mount_id: "public-mount".to_string(),
        source: MountSource::HostPath(PathBuf::from("/host/public")),
        access: MountAccess::ReadOnly,
        scope: MountScope::Session,
        target: PathBuf::from("workspace/mounts/public-mount"),
        env_var: None,
        source_snapshot: MountSourceSnapshot {
            exists: true,
            is_dir: true,
        },
    };
}
