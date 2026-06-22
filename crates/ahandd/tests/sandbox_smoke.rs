#![cfg(target_os = "macos")]

use std::collections::HashMap;
#[cfg(target_os = "macos")]
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use ahandd::{
    DaemonConfig,
    sandbox::{
        HostFileRef, NetworkPolicy, RegisterVersionRequest, RuntimeProviderConfig,
        SandboxExecRequest, SandboxPermissionMode, SandboxSessionConfig,
    },
};

fn command_stdout(program: &str, args: &[&str]) -> String {
    let output = Command::new(program).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{program} {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn python_executable() -> PathBuf {
    PathBuf::from(command_stdout(
        "python3",
        &["-c", "import sys; print(sys.executable)"],
    ))
}

fn node_executable() -> PathBuf {
    PathBuf::from(command_stdout("node", &["-p", "process.execPath"]))
}

fn runtime_root(executable: &Path) -> PathBuf {
    let canonical = executable
        .canonicalize()
        .unwrap_or_else(|_| executable.to_path_buf());
    if canonical.starts_with("/opt/homebrew") {
        return PathBuf::from("/opt/homebrew");
    }
    canonical
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("/usr"))
        .to_path_buf()
}

#[cfg(target_os = "macos")]
fn executable_shim(bin: &Path, command_name: &str, target: &Path) -> PathBuf {
    std::fs::create_dir_all(bin).unwrap();
    let shim = bin.join(command_name);
    symlink(target, &shim).unwrap();
    shim
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn coffice_sandbox_smoke_import_run_register_and_user_commit() {
    let temp = tempfile::tempdir().unwrap();
    let identity_dir = temp.path().join("identity");
    let sandbox_root = temp.path().join("sandbox");
    let python_runtime_bin = temp.path().join("runtime").join("python").join("bin");
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
        })
        .await
        .unwrap();

    let system_python = python_executable();
    let python = executable_shim(&python_runtime_bin, "python", &system_python);
    let node = node_executable();
    let mut python_env = HashMap::new();
    python_env.insert("PYTHONNOUSERSITE".to_string(), "1".to_string());
    handle
        .register_sandbox_runtime(
            "session-1",
            RuntimeProviderConfig {
                name: "python".to_string(),
                executable: python.clone(),
                readonly_roots: vec![runtime_root(&python), runtime_root(&system_python)],
                env: python_env,
                default_timeout: Duration::from_secs(10),
            },
        )
        .await
        .unwrap();
    handle
        .register_sandbox_runtime(
            "session-1",
            RuntimeProviderConfig {
                name: "node".to_string(),
                executable: node.clone(),
                readonly_roots: vec![runtime_root(&node)],
                env: HashMap::new(),
                default_timeout: Duration::from_secs(10),
            },
        )
        .await
        .unwrap();

    let imported = handle
        .import_sandbox_file(
            "session-1",
            HostFileRef {
                file_ref_id: "file-ref-1".to_string(),
                source_path: source.clone(),
                display_name: "source.txt".to_string(),
                size: 8,
                mtime_ms: None,
                conversation_id: Some("conversation-1".to_string()),
            },
        )
        .await
        .unwrap();

    let read_imported = handle
        .execute_sandbox_command(
            "session-1",
            SandboxExecRequest {
                command: vec![
                    "python".to_string(),
                    "-c".to_string(),
                    format!(
                        "from pathlib import Path; print(Path({:?}).read_text())",
                        imported.sandbox_path.to_string_lossy()
                    ),
                ],
                cwd: None,
                env: HashMap::new(),
                timeout: Some(Duration::from_secs(10)),
            },
        )
        .await
        .unwrap();
    assert_eq!(read_imported.exit_code, Some(0), "{}", read_imported.stderr);
    assert_eq!(read_imported.stdout.trim(), "original");

    let write_output = handle
        .execute_sandbox_command(
            "session-1",
            SandboxExecRequest {
                command: vec![
                    "node".to_string(),
                    "-e".to_string(),
                    "require('fs').writeFileSync('workspace/out.txt', 'changed')".to_string(),
                ],
                cwd: None,
                env: HashMap::new(),
                timeout: Some(Duration::from_secs(10)),
            },
        )
        .await
        .unwrap();
    assert_eq!(write_output.exit_code, Some(0), "{}", write_output.stderr);

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
    assert_eq!(
        version.status,
        ahandd::sandbox::FileVersionStatus::Candidate
    );
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "original");

    let agent_commit = handle
        .commit_sandbox_file_version("session-1", &version.version_id)
        .await
        .unwrap_err();
    assert_eq!(agent_commit.code, "PERMISSION_DENIED");
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "original");

    let user_commit = handle
        .confirm_sandbox_file_version_overwrite("session-1", &version.version_id)
        .await
        .unwrap();
    assert_eq!(user_commit.bytes_written, 7);
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "changed");

    handle.shutdown().await.unwrap();
}
