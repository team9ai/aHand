use std::ffi::OsString;
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
    "/Library/Apple",
    "/Library/Preferences",
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
];

pub async fn execute(request: PlatformExecuteRequest) -> SandboxResult<RuntimeExecuteResult> {
    if request.command.is_empty() {
        return Err(SandboxError::invalid_command(
            "sandbox command must not be empty",
        ));
    }
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
    for root in &policy.readonly_roots {
        sbpl.push_str(&format!(
            "(allow file-read* (subpath \"{}\"))\n",
            escape_sbpl(&root.to_string_lossy())
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
    if policy.network == NetworkPolicy::Enabled {
        sbpl.push_str("(allow network*)\n");
    }
    sbpl
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
            readonly_roots: vec![PathBuf::from("/runtimes/python")],
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
            readonly_roots: vec![PathBuf::from("/runtime/python")],
            network: NetworkPolicy::Enabled,
        };

        let sbpl = render_policy(&policy);

        assert!(sbpl.contains("(allow file-read* (subpath \"/runtime/python\"))"));
        assert!(sbpl.contains("(allow file-read* (subpath \"/sessions/s1\"))"));
        assert!(sbpl.contains("(allow file-write* (subpath \"/sessions/s1\"))"));
        assert!(!sbpl.contains("(allow file-write* (subpath \"/runtime/python\"))"));
        assert!(sbpl.contains("(allow network*"));
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
                readonly_roots: vec![PathBuf::from("/bin")],
                network: NetworkPolicy::Enabled,
            },
        })
        .await
        .unwrap();

        assert_ne!(result.exit_code, Some(0));
        assert!(!result.stdout.contains("root:"));
    }
}
