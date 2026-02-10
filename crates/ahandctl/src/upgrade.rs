use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

pub async fn run(check_only: bool, target_version: Option<String>) -> Result<()> {
    let script_path = resolve_script_path()?;

    if !script_path.exists() {
        anyhow::bail!(
            "upgrade.sh not found at {}\nRun: bash scripts/deploy-admin.sh",
            script_path.display()
        );
    }

    let mut cmd = Command::new("bash");
    cmd.arg(&script_path);

    if check_only {
        cmd.arg("--check");
    }
    if let Some(version) = target_version {
        cmd.arg("--version");
        cmd.arg(version);
    }

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().context("Failed to spawn upgrade.sh")?;

    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take().expect("stderr");

    let stdout_task = tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            println!("{}", line);
        }
    });

    let stderr_task = tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("{}", line);
        }
    });

    let status = child.wait().await?;
    let _ = stdout_task.await;
    let _ = stderr_task.await;

    if !status.success() {
        anyhow::bail!("Upgrade failed with exit code: {}", status);
    }

    Ok(())
}

fn resolve_script_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Failed to find home directory")?;
    Ok(home.join(".ahand").join("bin").join("upgrade.sh"))
}
