use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time;

use crate::sandbox::runner::{PlatformExecuteRequest, RuntimeSandboxPolicy};
use crate::sandbox::types::{NetworkPolicy, RuntimeExecuteResult, SandboxError, SandboxResult};

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
const SYSTEM_READONLY_ROOTS: &[&str] = &[
    "/bin",
    "/sbin",
    "/usr/bin",
    "/usr/lib",
    "/usr/libexec",
    "/usr/sbin",
    "/usr/share",
    "/opt/homebrew",
    "/usr/local",
    "/System/Library/CoreServices",
    "/System/Library/Extensions",
    "/System/Library/Frameworks",
    "/System/Library/PrivateFrameworks",
    "/System/Library/SubFrameworks",
    "/System/Volumes/Preboot/Cryptexes/OS",
    "/System/Library/OpenSSL",
    "/private/etc/ssl",
    "/etc/ssl",
    "/Library/Apple",
    "/Library/Preferences",
    "/Library/Developer/CommandLineTools",
    "/Applications/Xcode.app/Contents/Developer",
    "/Applications/Xcode.app/Contents/Frameworks",
    "/Applications/Xcode.app/Contents/SharedFrameworks",
];
const SYSTEM_EXECUTABLE_ROOTS: &[&str] = &[
    "/bin",
    "/sbin",
    "/usr/bin",
    "/usr/lib",
    "/usr/libexec",
    "/usr/sbin",
    "/opt/homebrew",
    "/usr/local",
    "/System/Library/Extensions",
    "/System/Library/Frameworks",
    "/System/Library/PrivateFrameworks",
    "/System/Library/SubFrameworks",
    "/System/Volumes/Preboot/Cryptexes/OS",
    "/Library/Apple",
    "/Library/Developer/CommandLineTools",
    "/Applications/Xcode.app/Contents/Developer",
    "/Applications/Xcode.app/Contents/Frameworks",
    "/Applications/Xcode.app/Contents/SharedFrameworks",
];

pub async fn execute(mut request: PlatformExecuteRequest) -> SandboxResult<RuntimeExecuteResult> {
    if request.command.is_empty() {
        return Err(SandboxError::invalid_command(
            "sandbox command must not be empty",
        ));
    }
    let writable_root = request.policy.writable_root.to_string_lossy().to_string();
    request
        .env
        .entry("HOME".to_string())
        .or_insert_with(|| writable_root.clone());
    request
        .env
        .entry("TMPDIR".to_string())
        .or_insert(writable_root);
    let policy = render_policy(&request.policy);
    let args = sandbox_exec_args(policy, &request.command);
    let mut command = Command::new(SANDBOX_EXEC);
    command
        .args(args)
        .current_dir(&request.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.env_clear();
    for (key, value) in request.env {
        command.env(key, value);
    }

    let mut child = command.spawn().map_err(|e| {
        SandboxError::unavailable(format!("failed to spawn sandboxed runtime: {e}"))
    })?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| SandboxError::unavailable("failed to capture sandboxed runtime stdout"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| SandboxError::unavailable("failed to capture sandboxed runtime stderr"))?;
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).to_string()
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).to_string()
    });

    let wait = time::timeout(request.timeout, child.wait()).await;
    let timed_out = wait.is_err();
    if timed_out {
        let _ = child.kill().await;
    }
    let exit_code = match wait {
        Ok(Ok(status)) => Some(status.code().unwrap_or(-1)),
        Ok(Err(e)) => {
            return Err(SandboxError::unavailable(format!(
                "failed waiting for sandboxed runtime: {e}"
            )));
        }
        Err(_) => None,
    };

    Ok(RuntimeExecuteResult {
        stdout: stdout_task.await.unwrap_or_default(),
        stderr: stderr_task.await.unwrap_or_default(),
        exit_code,
        timed_out,
    })
}

fn sandbox_exec_args(policy: String, command: &[String]) -> Vec<OsString> {
    let mut argv = vec![
        OsString::from("-p"),
        OsString::from(policy),
        OsString::from("--"),
    ];
    argv.extend(command.iter().map(OsString::from));
    argv
}

pub fn render_policy(policy: &RuntimeSandboxPolicy) -> String {
    let mut sbpl = String::from("(version 1)\n(deny default)\n");
    sbpl.push_str("(allow process-exec)\n");
    sbpl.push_str("(allow process-fork)\n");
    sbpl.push_str("(allow signal (target same-sandbox))\n");
    sbpl.push_str("(allow process-info* (target same-sandbox))\n");
    sbpl.push_str("(allow file-read-metadata)\n");
    sbpl.push_str("(allow file-read* (literal \"/\"))\n");
    sbpl.push_str("(allow sysctl-read)\n");
    for root in SYSTEM_READONLY_ROOTS {
        sbpl.push_str(&format!("(allow file-read* (subpath \"{root}\"))\n"));
    }
    for root in SYSTEM_EXECUTABLE_ROOTS {
        sbpl.push_str(&format!(
            "(allow file-map-executable (subpath \"{root}\"))\n"
        ));
    }
    let mut literal_read_paths = Vec::new();
    for root in &policy.readonly_roots {
        collect_read_literal_ancestors(&mut literal_read_paths, root);
        sbpl.push_str(&format!(
            "(allow file-read* (subpath \"{}\"))\n",
            escape_sbpl(&root.to_string_lossy())
        ));
        sbpl.push_str(&format!(
            "(allow file-map-executable (subpath \"{}\"))\n",
            escape_sbpl(&root.to_string_lossy())
        ));
    }
    collect_read_literal_ancestors(&mut literal_read_paths, &policy.writable_root);
    for root in &policy.writable_roots {
        collect_read_literal_ancestors(&mut literal_read_paths, root);
    }
    for path in literal_read_paths {
        sbpl.push_str(&format!(
            "(allow file-read* (literal \"{}\"))\n",
            escape_sbpl(&path)
        ));
    }
    sbpl.push_str(&format!(
        "(allow file-read* (subpath \"{}\"))\n",
        escape_sbpl(&policy.writable_root.to_string_lossy())
    ));
    sbpl.push_str(&format!(
        "(allow file-write* (subpath \"{}\"))\n",
        escape_sbpl(&policy.writable_root.to_string_lossy())
    ));
    for root in &policy.writable_roots {
        sbpl.push_str(&format!(
            "(allow file-read* (subpath \"{}\"))\n",
            escape_sbpl(&root.to_string_lossy())
        ));
        sbpl.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            escape_sbpl(&root.to_string_lossy())
        ));
    }
    append_common_device_rules(&mut sbpl);
    append_artifact_tool_socket_rules(&mut sbpl);
    if policy.network == NetworkPolicy::Enabled {
        sbpl.push_str("(allow network*)\n");
    }
    sbpl
}

fn collect_read_literal_ancestors(paths: &mut Vec<String>, path: &Path) {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
            Component::CurDir => continue,
            Component::ParentDir => current.push(component.as_os_str()),
            Component::Normal(part) => current.push(part),
        }

        let current = current.to_string_lossy();
        if current == "/" || current.is_empty() {
            continue;
        }
        push_unique_literal_path(paths, current.as_ref());
    }
}

fn push_unique_literal_path(paths: &mut Vec<String>, path: &str) {
    if !paths.iter().any(|existing| existing == path) {
        paths.push(path.to_string());
    }

    let trailing_path = format!("{}/", path.trim_end_matches('/'));
    if trailing_path != path && !paths.iter().any(|existing| existing == &trailing_path) {
        paths.push(trailing_path);
    }
}

fn append_common_device_rules(sbpl: &mut String) {
    sbpl.push_str("(allow file-read* (literal \"/dev/null\"))\n");
    sbpl.push_str("(allow file-write* (literal \"/dev/null\"))\n");
}

fn append_artifact_tool_socket_rules(sbpl: &mut String) {
    sbpl.push_str("(allow file-read* (literal \"/tmp\"))\n");
    sbpl.push_str("(allow file-read* (literal \"/private\"))\n");
    sbpl.push_str("(allow file-read* (literal \"/private/tmp\"))\n");
    sbpl.push_str("(allow file-write* (regex #\"^/tmp/artifact_tool_rpc_[^/]+\\.sock$\"))\n");
    sbpl.push_str(
        "(allow file-write* (regex #\"^/private/tmp/artifact_tool_rpc_[^/]+\\.sock$\"))\n",
    );
}

fn escape_sbpl(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::runner::{PlatformExecuteRequest, RuntimeSandboxPolicy};
    use crate::sandbox::types::NetworkPolicy;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn rendered_policy_allows_writable_root_and_runtime_reads() {
        let policy = RuntimeSandboxPolicy {
            writable_root: PathBuf::from("/sessions/s1"),
            writable_roots: Vec::new(),
            readonly_roots: vec![PathBuf::from("/runtimes/python")],
            mounts: Vec::new(),
            network: NetworkPolicy::Enabled,
        };

        let sbpl = render_policy(&policy);

        assert!(sbpl.contains("(allow file-read*"));
        assert!(sbpl.contains("/runtimes/python"));
        assert!(sbpl.contains("(allow file-write*"));
        assert!(sbpl.contains("/sessions/s1"));
        assert!(sbpl.contains("(allow network*"));
        assert!(sbpl.contains("(allow sysctl-read)"));
        assert!(sbpl.contains("(allow file-read* (literal \"/\"))"));
        assert!(!sbpl.contains("(subpath \"/etc\")"));
    }

    #[test]
    fn rendered_policy_allows_runtime_path_and_workspace_only_for_writes() {
        let policy = RuntimeSandboxPolicy {
            writable_root: PathBuf::from("/sessions/s1"),
            writable_roots: Vec::new(),
            readonly_roots: vec![PathBuf::from("/runtime/python")],
            mounts: Vec::new(),
            network: NetworkPolicy::Enabled,
        };

        let sbpl = render_policy(&policy);

        assert!(sbpl.contains("(allow file-read* (subpath \"/runtime/python\"))"));
        assert!(sbpl.contains("(allow file-map-executable (subpath \"/runtime/python\"))"));
        assert!(sbpl.contains("(allow file-read* (subpath \"/sessions/s1\"))"));
        assert!(sbpl.contains("(allow file-write* (subpath \"/sessions/s1\"))"));
        assert!(!sbpl.contains("(allow file-write* (subpath \"/runtime/python\"))"));
        assert!(!sbpl.contains("(allow file-map-executable (subpath \"/sessions/s1\"))"));
        assert!(sbpl.contains("(allow network*"));
    }

    #[test]
    fn rendered_policy_allows_literal_parent_reads_for_runtime_roots() {
        let policy = RuntimeSandboxPolicy {
            writable_root: PathBuf::from(
                "/Users/winrey/Library/Application Support/app/sessions/s1",
            ),
            writable_roots: Vec::new(),
            readonly_roots: vec![PathBuf::from(
                "/Users/winrey/Library/Application Support/app/python-sandbox/venv",
            )],
            mounts: Vec::new(),
            network: NetworkPolicy::Enabled,
        };

        let sbpl = render_policy(&policy);

        for literal in [
            "/Users",
            "/Users/winrey",
            "/Users/winrey/Library",
            "/Users/winrey/Library/Application Support",
            "/Users/winrey/Library/Application Support/app",
            "/Users/winrey/Library/Application Support/app/python-sandbox",
            "/Users/winrey/Library/Application Support/app/python-sandbox/venv",
            "/Users/winrey/Library/Application Support/app/sessions",
            "/Users/winrey/Library/Application Support/app/sessions/s1",
        ] {
            assert!(
                sbpl.contains(&format!("(allow file-read* (literal \"{literal}\"))")),
                "missing literal read for {literal}\n{sbpl}"
            );
        }

        assert!(!sbpl.contains("(allow file-read* (subpath \"/Users\"))"));
    }

    #[test]
    fn rendered_policy_allows_only_artifact_tool_tmp_sockets() {
        let policy = RuntimeSandboxPolicy {
            writable_root: PathBuf::from("/sessions/s1"),
            writable_roots: Vec::new(),
            readonly_roots: vec![PathBuf::from("/runtime/python")],
            mounts: Vec::new(),
            network: NetworkPolicy::Enabled,
        };

        let sbpl = render_policy(&policy);

        assert!(sbpl.contains("(allow file-read* (literal \"/tmp\"))"));
        assert!(sbpl.contains("(allow file-read* (literal \"/private\"))"));
        assert!(sbpl.contains("(allow file-read* (literal \"/private/tmp\"))"));
        assert!(
            sbpl.contains("(allow file-write* (regex #\"^/tmp/artifact_tool_rpc_[^/]+\\.sock$\"))")
        );
        assert!(sbpl.contains(
            "(allow file-write* (regex #\"^/private/tmp/artifact_tool_rpc_[^/]+\\.sock$\"))"
        ));
        assert!(!sbpl.contains("(allow file-write* (subpath \"/tmp\"))"));
        assert!(!sbpl.contains("(allow file-write* (subpath \"/private/tmp\"))"));
    }

    #[test]
    fn rendered_policy_allows_null_device_for_common_unix_tools() {
        let policy = RuntimeSandboxPolicy {
            writable_root: PathBuf::from("/sessions/s1"),
            writable_roots: Vec::new(),
            readonly_roots: Vec::new(),
            mounts: Vec::new(),
            network: NetworkPolicy::Enabled,
        };

        let sbpl = render_policy(&policy);

        assert!(sbpl.contains("(allow file-read* (literal \"/dev/null\"))"));
        assert!(sbpl.contains("(allow file-write* (literal \"/dev/null\"))"));
        assert!(!sbpl.contains("(allow file-read* (subpath \"/dev\"))"));
        assert!(!sbpl.contains("(allow file-write* (subpath \"/dev\"))"));
    }

    #[test]
    fn rendered_policy_allows_macos_tls_config_for_system_curl() {
        let policy = RuntimeSandboxPolicy {
            writable_root: PathBuf::from("/sessions/s1"),
            writable_roots: Vec::new(),
            readonly_roots: vec![PathBuf::from("/runtime/python")],
            mounts: Vec::new(),
            network: NetworkPolicy::Enabled,
        };

        let sbpl = render_policy(&policy);

        assert!(sbpl.contains("(allow network*)"));
        assert!(sbpl.contains("(allow file-read* (subpath \"/private/etc/ssl\"))"));
        assert!(sbpl.contains("(allow file-read* (subpath \"/etc/ssl\"))"));
        assert!(sbpl.contains("(allow file-read* (subpath \"/System/Library/OpenSSL\"))"));
    }

    #[test]
    fn rendered_policy_allows_apple_developer_tools_for_usr_bin_git() {
        let policy = RuntimeSandboxPolicy {
            writable_root: PathBuf::from("/sessions/s1"),
            writable_roots: Vec::new(),
            readonly_roots: vec![PathBuf::from("/runtime/python")],
            mounts: Vec::new(),
            network: NetworkPolicy::Enabled,
        };

        let sbpl = render_policy(&policy);

        for root in [
            "/Library/Developer/CommandLineTools",
            "/Applications/Xcode.app/Contents/Developer",
            "/Applications/Xcode.app/Contents/Frameworks",
            "/Applications/Xcode.app/Contents/SharedFrameworks",
        ] {
            assert!(
                sbpl.contains(&format!("(allow file-read* (subpath \"{root}\"))")),
                "missing developer tool read root for {root}\n{sbpl}"
            );
            assert!(
                sbpl.contains(&format!("(allow file-map-executable (subpath \"{root}\"))")),
                "missing developer tool executable mapping root for {root}\n{sbpl}"
            );
        }
    }

    #[test]
    fn sandbox_exec_argv_separates_policy_from_sandboxed_command() {
        let argv = sandbox_exec_args(
            "(version 1)".to_string(),
            &[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo ok".to_string(),
            ],
        );
        let argv = argv
            .iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();

        assert_eq!(argv[0], "-p");
        assert_eq!(argv[1], "(version 1)");
        assert_eq!(argv[2], "--");
        assert_eq!(argv[3], "/bin/sh");
        assert_eq!(argv[4], "-c");
        assert_eq!(argv[5], "echo ok");
    }

    #[tokio::test]
    #[ignore]
    async fn macos_runtime_denies_outside_read() {
        let temp = tempfile::tempdir().unwrap();
        let result = execute(PlatformExecuteRequest {
            command: vec!["/bin/sh".into(), "-c".into(), "/bin/cat /etc/passwd".into()],
            cwd: temp.path().to_path_buf(),
            env: HashMap::new(),
            timeout: Duration::from_secs(5),
            policy: RuntimeSandboxPolicy {
                writable_root: temp.path().to_path_buf(),
                writable_roots: Vec::new(),
                readonly_roots: vec![PathBuf::from("/bin")],
                mounts: Vec::new(),
                network: NetworkPolicy::Enabled,
            },
            sandbox_state_root: temp.path().join("windows-sandbox"),
        })
        .await
        .unwrap();

        assert_ne!(result.exit_code, Some(0));
        assert!(!result.stdout.contains("root:"));
    }

    #[tokio::test]
    async fn macos_runtime_defaults_home_and_tmpdir_to_writable_root() {
        let temp = tempfile::tempdir().unwrap();
        let result = execute(PlatformExecuteRequest {
            command: vec!["/usr/bin/env".into()],
            cwd: temp.path().to_path_buf(),
            env: HashMap::new(),
            timeout: Duration::from_secs(5),
            policy: RuntimeSandboxPolicy {
                writable_root: temp.path().to_path_buf(),
                writable_roots: Vec::new(),
                readonly_roots: vec![PathBuf::from("/usr/bin")],
                mounts: Vec::new(),
                network: NetworkPolicy::Enabled,
            },
            sandbox_state_root: temp.path().join("windows-sandbox"),
        })
        .await
        .unwrap();

        assert_eq!(result.exit_code, Some(0));
        assert!(
            result
                .stdout
                .contains(&format!("HOME={}", temp.path().to_string_lossy()))
        );
        assert!(
            result
                .stdout
                .contains(&format!("TMPDIR={}", temp.path().to_string_lossy()))
        );
    }
}
