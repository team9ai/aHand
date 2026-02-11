use anyhow::{Context, Result};
use std::path::PathBuf;

fn get_data_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Failed to find home directory")?;
    Ok(home.join(".ahand").join("data"))
}

fn get_pid_path() -> Result<PathBuf> {
    Ok(get_data_dir()?.join("daemon.pid"))
}

fn get_log_path() -> Result<PathBuf> {
    Ok(get_data_dir()?.join("daemon.log"))
}

/// Find the ahandd binary: installed path → sibling of current exe → error.
fn find_ahandd_binary() -> Result<PathBuf> {
    // 1. Installed location: ~/.ahand/bin/ahandd
    if let Some(home) = dirs::home_dir() {
        let installed = home.join(".ahand").join("bin").join("ahandd");
        if installed.exists() {
            return Ok(installed);
        }
    }

    // 2. Sibling of current executable (dev builds: target/debug/)
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let sibling = dir.join("ahandd");
            if sibling.exists() {
                return Ok(sibling);
            }
        }
    }

    anyhow::bail!(
        "Cannot find ahandd binary. Expected at ~/.ahand/bin/ahandd or next to ahandctl."
    )
}

/// Read PID file and check if the process is still alive.
fn read_running_pid() -> Result<Option<u32>> {
    let pid_path = get_pid_path()?;
    if !pid_path.exists() {
        return Ok(None);
    }
    let pid_str = std::fs::read_to_string(&pid_path)?;
    let pid: u32 = pid_str.trim().parse().context("Invalid PID in daemon.pid")?;
    if is_process_running(pid) {
        Ok(Some(pid))
    } else {
        // Stale PID file
        let _ = std::fs::remove_file(&pid_path);
        Ok(None)
    }
}

#[cfg(target_os = "linux")]
fn is_process_running(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{}", pid)).exists()
}

#[cfg(not(target_os = "linux"))]
fn is_process_running(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn send_signal(pid: u32, sig: &str) -> Result<()> {
    let status = std::process::Command::new("kill")
        .args([sig, &pid.to_string()])
        .status()
        .context("Failed to run kill command")?;
    if !status.success() {
        anyhow::bail!("kill {} {} failed", sig, pid);
    }
    Ok(())
}

pub async fn start(config: Option<String>) -> Result<()> {
    if let Some(pid) = read_running_pid()? {
        println!("Daemon is already running (PID {}).", pid);
        return Ok(());
    }

    let ahandd = find_ahandd_binary()?;
    let log_path = get_log_path()?;
    let data_dir = get_data_dir()?;

    std::fs::create_dir_all(&data_dir)?;

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("Failed to open log file: {}", log_path.display()))?;
    let log_file_err = log_file.try_clone()?;

    let mut cmd = std::process::Command::new(&ahandd);

    if let Some(cfg) = &config {
        cmd.arg("--config").arg(cfg);
    }

    cmd.stdout(log_file);
    cmd.stderr(log_file_err);
    cmd.stdin(std::process::Stdio::null());

    // Detach into a new process group so it survives terminal close.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let child = cmd
        .spawn()
        .with_context(|| format!("Failed to start daemon: {}", ahandd.display()))?;

    let pid = child.id();
    println!("Daemon started (PID {}).", pid);
    println!("Log file: {}", log_path.display());

    // Brief wait to verify it didn't exit immediately.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    if !is_process_running(pid) {
        eprintln!("Warning: daemon may have exited immediately. Check logs:");
        eprintln!("  {}", log_path.display());
        std::process::exit(1);
    }

    Ok(())
}

pub async fn stop() -> Result<()> {
    let pid = match read_running_pid()? {
        Some(pid) => pid,
        None => {
            println!("Daemon is not running.");
            return Ok(());
        }
    };

    println!("Stopping daemon (PID {})...", pid);

    if let Err(e) = send_signal(pid, "-TERM") {
        eprintln!("Failed to send SIGTERM: {}", e);
    }

    // Wait for process to exit (up to 10 seconds).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if !is_process_running(pid) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            eprintln!("Daemon did not stop within 10s, sending SIGKILL...");
            let _ = send_signal(pid, "-KILL");
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    // Clean up stale PID file if the daemon didn't remove it.
    let pid_path = get_pid_path()?;
    if pid_path.exists() {
        let _ = std::fs::remove_file(&pid_path);
    }

    println!("Daemon stopped.");
    Ok(())
}

pub async fn restart(config: Option<String>) -> Result<()> {
    stop().await?;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    start(config).await
}

pub async fn status() -> Result<()> {
    match read_running_pid()? {
        Some(pid) => println!("Daemon is running (PID {}).", pid),
        None => println!("Daemon is not running."),
    }
    Ok(())
}
