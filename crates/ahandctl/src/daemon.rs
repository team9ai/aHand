use ahand_platform::process::{self, TerminateMode};
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
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    find_ahandd_binary_in(dirs::home_dir().as_deref(), exe_dir.as_deref())
}

/// Testable seam: resolve ahandd binary given explicit home and exe-dir.
///
/// Search order:
/// 1. `home/.ahand/bin/ahandd[.exe]` (installed location)
/// 2. `exe_dir/ahandd[.exe]`          (sibling of current executable, dev builds)
/// 3. Error
pub(crate) fn find_ahandd_binary_in(
    home: Option<&std::path::Path>,
    exe_dir: Option<&std::path::Path>,
) -> Result<PathBuf> {
    let bin = ahand_platform::paths::exe_name("ahandd");

    // 1. Installed location: ~/.ahand/bin/ahandd[.exe]
    if let Some(h) = home {
        let installed = h.join(".ahand").join("bin").join(&bin);
        if installed.exists() {
            return Ok(installed);
        }
    }

    // 2. Sibling of current executable (dev builds: target/debug/)
    if let Some(dir) = exe_dir {
        let sibling = dir.join(&bin);
        if sibling.exists() {
            return Ok(sibling);
        }
    }

    anyhow::bail!("Cannot find ahandd binary. Expected at ~/.ahand/bin/{bin} or next to ahandctl.")
}

/// Read PID file and check if the process is still alive.
fn read_running_pid() -> Result<Option<u32>> {
    let pid_path = get_pid_path()?;
    read_running_pid_at(&pid_path)
}

/// Testable seam: read and validate a PID file at an explicit path.
///
/// - Missing file → `Ok(None)`
/// - File contains a running PID → `Ok(Some(pid))`
/// - File contains a dead PID → `Ok(None)` and file is removed (stale cleanup)
/// - Garbage content → `Err`
pub(crate) fn read_running_pid_at(pid_path: &std::path::Path) -> Result<Option<u32>> {
    if !pid_path.exists() {
        return Ok(None);
    }
    let pid_str = std::fs::read_to_string(pid_path)?;
    let pid: u32 = pid_str
        .trim()
        .parse()
        .context("Invalid PID in daemon.pid")?;
    if process::is_process_running(pid) {
        Ok(Some(pid))
    } else {
        // Stale PID file
        let _ = std::fs::remove_file(pid_path);
        Ok(None)
    }
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

    // Detach so the daemon survives terminal/console close.
    process::configure_detached(&mut cmd);

    let child = cmd
        .spawn()
        .with_context(|| format!("Failed to start daemon: {}", ahandd.display()))?;

    let pid = child.id();
    println!("Daemon started (PID {}).", pid);
    println!("Log file: {}", log_path.display());

    // Brief wait to verify it didn't exit immediately.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    if !process::is_process_running(pid) {
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

    if let Err(e) = process::terminate(pid, TerminateMode::Graceful) {
        eprintln!("Failed to request stop: {e}");
    }

    // Wait for process to exit (up to 10 seconds).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if !process::is_process_running(pid) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            eprintln!("Daemon did not stop within 10s, force-killing...");
            let _ = process::terminate(pid, TerminateMode::Force);
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── find_ahandd_binary_in ─────────────────────────────────────────────────

    #[test]
    fn find_ahandd_binary_in_installed_hit() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_name = ahand_platform::paths::exe_name("ahandd");
        let bin_dir = tmp.path().join(".ahand").join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin_path = bin_dir.join(&bin_name);
        std::fs::write(&bin_path, b"fake").unwrap();

        let result = find_ahandd_binary_in(Some(tmp.path()), None).unwrap();
        assert_eq!(result, bin_path);
    }

    #[test]
    fn find_ahandd_binary_in_sibling_hit_when_installed_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_name = ahand_platform::paths::exe_name("ahandd");
        let sibling_path = tmp.path().join(&bin_name);
        std::fs::write(&sibling_path, b"fake").unwrap();

        // No installed binary — only the sibling.
        let empty_home = tempfile::tempdir().unwrap();
        let result = find_ahandd_binary_in(Some(empty_home.path()), Some(tmp.path())).unwrap();
        assert_eq!(result, sibling_path);
    }

    #[test]
    fn find_ahandd_binary_in_neither_errors_with_expected_path_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let err = find_ahandd_binary_in(Some(tmp.path()), Some(tmp.path())).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("ahandd") || msg.contains("Cannot find"),
            "error should mention ahandd binary: {msg}"
        );
    }

    // ── read_running_pid_at ───────────────────────────────────────────────────

    #[test]
    fn read_running_pid_at_no_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let pid_path = tmp.path().join("daemon.pid");
        // File does not exist.
        let result = read_running_pid_at(&pid_path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_running_pid_at_current_process_returns_some() {
        let tmp = tempfile::tempdir().unwrap();
        let pid_path = tmp.path().join("daemon.pid");
        let pid = std::process::id();
        std::fs::write(&pid_path, format!("{pid}\n")).unwrap();

        let result = read_running_pid_at(&pid_path).unwrap();
        assert_eq!(result, Some(pid));
        // File should still exist (live process).
        assert!(pid_path.exists());
    }

    #[test]
    fn read_running_pid_at_dead_pid_returns_none_and_removes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let pid_path = tmp.path().join("daemon.pid");
        // u32::MAX - 7 is almost certainly not a running PID.
        let dead_pid = u32::MAX - 7;
        std::fs::write(&pid_path, format!("{dead_pid}\n")).unwrap();

        let result = read_running_pid_at(&pid_path).unwrap();
        assert!(result.is_none(), "dead pid should return None");
        assert!(
            !pid_path.exists(),
            "stale PID file should be removed after dead-pid detection"
        );
    }

    #[test]
    fn read_running_pid_at_garbage_content_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let pid_path = tmp.path().join("daemon.pid");
        std::fs::write(&pid_path, b"not-a-pid\n").unwrap();

        let err = read_running_pid_at(&pid_path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Invalid PID") || msg.contains("invalid digit"),
            "garbage pid file should error: {msg}"
        );
    }
}
