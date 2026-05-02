use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};

use super::node::Dirs;
use super::types::{
    CheckReport, CheckSource, CheckStatus, FixHint, LogStream, Phase, ProgressEvent,
};

pub const PLAYWRIGHT_CLI_VERSION: &str = "0.1.1";

fn cli_path() -> Result<PathBuf> {
    let dirs = Dirs::new()?;
    Ok(dirs.node.join("bin").join("playwright-cli"))
}

/// Read-only check: report current playwright-cli status.
pub async fn inspect() -> CheckReport {
    let Ok(cli) = cli_path() else {
        return missing_report();
    };

    if !cli.exists() {
        return missing_report();
    }

    let output = tokio::process::Command::new(&cli)
        .arg("--version")
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
            CheckReport {
                name: "playwright",
                label: "playwright-cli",
                status: CheckStatus::Ok {
                    version,
                    path: cli,
                    source: CheckSource::Managed,
                },
                fix_hint: None,
            }
        }
        _ => missing_report(),
    }
}

fn missing_report() -> CheckReport {
    CheckReport {
        name: "playwright",
        label: "playwright-cli",
        status: CheckStatus::Missing,
        fix_hint: Some(FixHint::RunStep {
            command: "ahandd browser-init --step playwright".into(),
        }),
    }
}

/// Spawn an npm command with piped stdout/stderr, forwarding each line to
/// the progress callback as `Phase::Log` events. Returns `Ok(())` on
/// successful exit; `Err(anyhow::Error)` with the combined stderr tail
/// on non-zero exit (so `classify_error` continues to see the same
/// failure strings).
///
/// `program` is the full path to the npm binary.
/// `args` are the arguments to pass to npm (e.g. `["install", "-g", ...]`).
async fn spawn_npm_with_progress(
    program: &std::path::Path,
    args: &[&str],
    progress: &(dyn Fn(ProgressEvent) + Send + Sync),
) -> anyhow::Result<()> {
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn npm: {e}"))?;

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    let mut stdout_lines = BufReader::new(stdout).lines();
    let mut stderr_lines = BufReader::new(stderr).lines();

    // Collect a bounded stderr tail for the bail message while streaming
    // both stdout and stderr concurrently via tokio::select!.
    let mut stderr_tail: Vec<String> = Vec::new();
    let mut stdout_done = false;
    let mut stderr_done = false;

    while !stdout_done || !stderr_done {
        tokio::select! {
            line = stdout_lines.next_line(), if !stdout_done => {
                match line {
                    Ok(Some(l)) => progress(ProgressEvent {
                        step: "playwright",
                        phase: Phase::Log,
                        message: l,
                        percent: None,
                        stream: Some(LogStream::Stdout),
                    }),
                    Ok(None) => stdout_done = true,
                    Err(e) => {
                        stdout_done = true;
                        progress(ProgressEvent {
                            step: "playwright",
                            phase: Phase::Log,
                            message: format!("<stdout read error: {e}>"),
                            percent: None,
                            stream: Some(LogStream::Info),
                        });
                    }
                }
            }
            line = stderr_lines.next_line(), if !stderr_done => {
                match line {
                    Ok(Some(l)) => {
                        // Keep a bounded tail (last 40 lines) for the bail message.
                        if stderr_tail.len() >= 40 {
                            stderr_tail.remove(0);
                        }
                        stderr_tail.push(l.clone());
                        progress(ProgressEvent {
                            step: "playwright",
                            phase: Phase::Log,
                            message: l,
                            percent: None,
                            stream: Some(LogStream::Stderr),
                        });
                    }
                    Ok(None) => stderr_done = true,
                    Err(e) => {
                        stderr_done = true;
                        progress(ProgressEvent {
                            step: "playwright",
                            phase: Phase::Log,
                            message: format!("<stderr read error: {e}>"),
                            percent: None,
                            stream: Some(LogStream::Info),
                        });
                    }
                }
            }
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| anyhow::anyhow!("failed to wait for npm: {e}"))?;

    if !status.success() {
        let tail = stderr_tail.join("\n");
        anyhow::bail!(
            "Failed to run npm {} (exit {}):\n{tail}",
            args.first().copied().unwrap_or(""),
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

/// Ensure playwright-cli is installed at the pinned version.
/// If `force`, uninstall first and reinstall.
pub async fn ensure(
    force: bool,
    progress: &(dyn Fn(ProgressEvent) + Send + Sync),
) -> Result<CheckReport> {
    let dirs = Dirs::new()?;
    let npm = dirs.node.join("bin").join("npm");
    if !npm.exists() {
        anyhow::bail!(
            "npm not found at {} — install Node.js first (`ahandd browser-init --step node`)",
            npm.display()
        );
    }
    let cli = cli_path()?;
    let prefix = dirs.node.to_string_lossy().to_string();

    if force && cli.exists() {
        emit(
            progress,
            Phase::Starting,
            "Uninstalling existing playwright-cli".into(),
        );
        // Best-effort: ignore errors so that a partially-broken install
        // doesn't block the reinstall.
        let _ = spawn_npm_with_progress(
            &npm,
            &["uninstall", "-g", "--prefix", &prefix, "@playwright/cli"],
            progress,
        )
        .await;
    }

    // Check cache (skip if unchanged and not forced)
    if !force && cli.exists() {
        if let Ok(out) = tokio::process::Command::new(&cli)
            .arg("--version")
            .output()
            .await
        {
            if out.status.success() {
                let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
                emit(
                    progress,
                    Phase::Done,
                    format!("playwright-cli {ver} already installed"),
                );
                return Ok(inspect().await);
            }
        }
    }

    emit(
        progress,
        Phase::Installing,
        format!("Installing @playwright/cli@{PLAYWRIGHT_CLI_VERSION}"),
    );

    spawn_npm_with_progress(
        &npm,
        &[
            "install",
            "-g",
            "--prefix",
            &prefix,
            &format!("@playwright/cli@{PLAYWRIGHT_CLI_VERSION}"),
        ],
        progress,
    )
    .await
    .context("npm install failed")?;

    emit(
        progress,
        Phase::Verifying,
        "Verifying playwright-cli".into(),
    );

    if !cli.exists() {
        anyhow::bail!(
            "playwright-cli was installed but binary not found at {}",
            cli.display()
        );
    }

    let version = tokio::process::Command::new(&cli)
        .arg("--version")
        .output()
        .await
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "installed".to_string());

    emit(
        progress,
        Phase::Done,
        format!("playwright-cli {version} ready"),
    );

    Ok(inspect().await)
}

fn emit(progress: &(dyn Fn(ProgressEvent) + Send + Sync), phase: Phase, message: String) {
    progress(ProgressEvent {
        step: "playwright",
        phase,
        message,
        percent: None,
        stream: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex};

    // ---------------------------------------------------------------------------
    // spawn_npm_with_progress tests
    // ---------------------------------------------------------------------------

    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_npm_install_forwards_stdout_stderr_lines() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("fake-npm.sh");
        std::fs::write(
            &script_path,
            "#!/bin/sh\n\
             echo 'npm notice created a lockfile'\n\
             echo 'npm notice cleaned up node_modules'\n\
             echo 'npm warn deprecated foo@1.2.3' >&2\n\
             echo 'npm warn deprecated bar@4.5.6' >&2\n\
             exit 0\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).unwrap();

        let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_cb = events.clone();
        let cb = move |e: ProgressEvent| {
            events_cb.lock().unwrap().push(e);
        };

        let result = spawn_npm_with_progress(
            &script_path,
            &[
                "install",
                "-g",
                "--prefix",
                "/tmp/unused-prefix",
                "fake@0.0.0",
            ],
            &cb,
        )
        .await;

        assert!(result.is_ok(), "expected success, got {:?}", result.err());

        let events = events.lock().unwrap();
        let stdout_lines: Vec<&str> = events
            .iter()
            .filter(|e| matches!(e.stream, Some(LogStream::Stdout)))
            .map(|e| e.message.as_str())
            .collect();
        let stderr_lines: Vec<&str> = events
            .iter()
            .filter(|e| matches!(e.stream, Some(LogStream::Stderr)))
            .map(|e| e.message.as_str())
            .collect();

        assert_eq!(
            stdout_lines,
            vec![
                "npm notice created a lockfile",
                "npm notice cleaned up node_modules",
            ]
        );
        assert_eq!(
            stderr_lines,
            vec![
                "npm warn deprecated foo@1.2.3",
                "npm warn deprecated bar@4.5.6",
            ]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_npm_install_surfaces_nonzero_exit_in_bail() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("fake-npm.sh");
        std::fs::write(
            &script_path,
            "#!/bin/sh\n\
             echo 'npm ERR! EACCES: permission denied' >&2\n\
             echo 'npm ERR! fix this by running chown' >&2\n\
             exit 243\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).unwrap();

        let cb = |_: ProgressEvent| {};
        let result = spawn_npm_with_progress(
            &script_path,
            &[
                "install",
                "-g",
                "--prefix",
                "/tmp/unused-prefix",
                "fake@0.0.0",
            ],
            &cb,
        )
        .await;

        assert!(result.is_err());
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains("243"),
            "bail message should include exit code: {msg}"
        );
        assert!(
            msg.contains("EACCES"),
            "bail message should contain stderr tail for classify_error: {msg}"
        );
    }

    // ---------------------------------------------------------------------------
    // Existing tests
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn inspect_returns_missing_when_cli_absent() {
        // This test is environment-dependent: it checks that if the user's
        // ~/.ahand/node/bin/playwright-cli does NOT exist, inspect() returns Missing.
        // Skip if it happens to exist on the test machine.
        let Ok(cli) = cli_path() else {
            eprintln!("skipping: cannot determine home directory");
            return;
        };
        if cli.exists() {
            eprintln!("skipping: {} already exists", cli.display());
            return;
        }
        let report = inspect().await;
        assert_eq!(report.name, "playwright");
        assert_eq!(report.label, "playwright-cli");
        assert!(matches!(report.status, CheckStatus::Missing));
        assert!(matches!(report.fix_hint, Some(FixHint::RunStep { .. })));
    }

    #[test]
    fn missing_report_has_correct_fix_hint() {
        let report = missing_report();
        assert_eq!(report.name, "playwright");
        assert_eq!(report.label, "playwright-cli");
        assert!(matches!(report.status, CheckStatus::Missing));
        match &report.fix_hint {
            Some(FixHint::RunStep { command }) => {
                assert!(command.contains("playwright"));
            }
            _ => panic!("expected RunStep fix hint"),
        }
    }
}
