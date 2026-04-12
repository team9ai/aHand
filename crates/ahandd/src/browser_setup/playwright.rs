use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result};

use super::node::Dirs;
use super::types::{CheckReport, CheckSource, CheckStatus, FixHint, Phase, ProgressEvent};

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
        let _ = tokio::process::Command::new(&npm)
            .args(["uninstall", "-g", "--prefix", &prefix, "@playwright/cli"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
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

    let output = tokio::process::Command::new(&npm)
        .args([
            "install",
            "-g",
            "--prefix",
            &prefix,
            &format!("@playwright/cli@{PLAYWRIGHT_CLI_VERSION}"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("failed to run npm install")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();

        if stderr.contains("EACCES") || stderr.contains("permission denied") {
            anyhow::bail!(
                "Permission denied installing playwright-cli to {prefix}. \
                 Check directory permissions: chmod -R u+w {prefix}"
            );
        }
        if stderr.contains("ETIMEDOUT")
            || stderr.contains("ENOTFOUND")
            || stderr.contains("ECONNREFUSED")
            || stderr.contains("network")
            || stderr.contains("fetch failed")
        {
            anyhow::bail!(
                "Network error installing playwright-cli. \
                 Check your internet connection and proxy settings."
            );
        }
        if stderr.contains("404") || stderr.contains("Not Found") {
            anyhow::bail!(
                "Package @playwright/cli@{PLAYWRIGHT_CLI_VERSION} not found on npm registry. \
                 The version may have been unpublished."
            );
        }
        anyhow::bail!(
            "Failed to install @playwright/cli@{PLAYWRIGHT_CLI_VERSION} (exit {}):\n{}",
            output.status.code().unwrap_or(-1),
            stderr,
        );
    }

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
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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
