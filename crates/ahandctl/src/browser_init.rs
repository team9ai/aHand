use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

pub async fn run(force: bool) -> Result<()> {
    let script_path = resolve_script_path()?;

    if !script_path.exists() {
        anyhow::bail!(
            "setup-browser.sh not found at {}\nRun: bash scripts/deploy-admin.sh",
            script_path.display()
        );
    }

    println!("Running browser setup...");
    println!();

    let mut cmd = Command::new("bash");
    cmd.arg(&script_path);
    cmd.arg("--from-release");

    if force {
        // Force reinstall by cleaning first
        println!("Force mode: cleaning existing installation...");
        let _ = Command::new("bash")
            .arg(&script_path)
            .arg("--clean")
            .status()
            .await;
    }

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().context("Failed to spawn setup-browser.sh")?;

    // Stream stdout
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
        anyhow::bail!("Browser setup failed with exit code: {}", status);
    }

    Ok(())
}

fn resolve_script_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Failed to find home directory")?;
    Ok(home.join(".ahand").join("bin").join("setup-browser.sh"))
}
