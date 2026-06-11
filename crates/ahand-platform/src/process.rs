//! Cross-platform process lifecycle: detached spawn, liveness, termination.
//!
//! Windows has no SIGTERM. `TerminateMode::Graceful` is therefore only a
//! *request* level on Unix (SIGTERM); on Windows both modes hard-kill via
//! `taskkill` (`/F` for Force). Callers that need graceful shutdown on
//! Windows must use an application-level channel (e.g. IPC shutdown message).

use anyhow::{Context, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminateMode {
    /// SIGTERM on Unix; `taskkill /PID n` (no /F) on Windows.
    Graceful,
    /// SIGKILL on Unix; `taskkill /F /PID n` on Windows.
    Force,
}

/// Configure a command to run detached from the current terminal/console so
/// it survives the parent exiting (new process group on Unix; detached,
/// windowless process on Windows).
pub fn configure_detached(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
}

#[cfg(unix)]
pub fn is_process_running(pid: u32) -> bool {
    // kill(pid, 0) probes existence without signaling. EPERM means it exists
    // but belongs to another user.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
pub fn is_process_running(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|output| {
            let stdout = String::from_utf8_lossy(&output.stdout);
            output.status.success()
                && stdout
                    .split_whitespace()
                    .any(|w| w == pid.to_string().as_str())
        })
        .unwrap_or(false)
}

#[cfg(unix)]
pub fn terminate(pid: u32, mode: TerminateMode) -> Result<()> {
    let sig = match mode {
        TerminateMode::Graceful => libc::SIGTERM,
        TerminateMode::Force => libc::SIGKILL,
    };
    let r = unsafe { libc::kill(pid as libc::pid_t, sig) };
    if r != 0 {
        let e = std::io::Error::last_os_error();
        // ESRCH: already gone — that's success for our purposes.
        if e.raw_os_error() != Some(libc::ESRCH) {
            return Err(e).context(format!("kill({pid})"));
        }
    }
    Ok(())
}

#[cfg(windows)]
pub fn terminate(pid: u32, mode: TerminateMode) -> Result<()> {
    let mut cmd = std::process::Command::new("taskkill");
    if matches!(mode, TerminateMode::Force) {
        cmd.arg("/F");
    }
    let output = cmd
        .args(["/PID", &pid.to_string()])
        .output()
        .context("failed to run taskkill")?;
    if !output.status.success() {
        let msg = String::from_utf8_lossy(&output.stderr);
        // "not found" (process already gone) counts as success.
        if !msg.contains("not found") && !msg.contains("128") {
            anyhow::bail!("taskkill /PID {pid} failed: {msg}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_running() {
        assert!(is_process_running(std::process::id()));
    }

    #[test]
    fn nonexistent_pid_is_not_running() {
        // PID near the top of the range; collision chance negligible.
        assert!(!is_process_running(u32::MAX - 7));
    }

    #[test]
    fn terminate_kills_a_spawned_child() {
        let mut cmd = std::process::Command::new(sleep_cmd());
        sleep_args(&mut cmd);
        let mut child = cmd.spawn().expect("spawn sleeper");
        let pid = child.id();
        assert!(is_process_running(pid));
        terminate(pid, TerminateMode::Force).expect("terminate");
        // Allow the OS a moment to reap, and actively reap the child to avoid
        // zombie semantics on macOS where kill(pid, 0) returns 0 for zombies.
        for _ in 0..50 {
            let _ = child.try_wait();
            if !is_process_running(pid) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("process {pid} still running after terminate");
    }

    fn sleep_cmd() -> &'static str {
        if cfg!(windows) { "cmd.exe" } else { "sleep" }
    }

    fn sleep_args(cmd: &mut std::process::Command) {
        if cfg!(windows) {
            cmd.args(["/C", "ping -n 60 127.0.0.1 >NUL"]);
        } else {
            cmd.arg("60");
        }
    }

    // ── Graceful terminate (#2) ───────────────────────────────────────────────

    #[test]
    fn terminate_graceful_stops_a_spawned_sleeper() {
        let mut cmd = std::process::Command::new(sleep_cmd());
        sleep_args(&mut cmd);
        let mut child = cmd.spawn().expect("spawn sleeper");
        let pid = child.id();
        assert!(
            is_process_running(pid),
            "process should be running before terminate"
        );

        terminate(pid, TerminateMode::Graceful).expect("graceful terminate");

        // On Unix, SIGTERM requests termination; on Windows taskkill /PID (no /F)
        // sends a WM_CLOSE / console event — not all console-less processes
        // honor it immediately. Wait up to 5 s and reap the child.
        for _ in 0..50 {
            let _ = child.try_wait();
            if !is_process_running(pid) {
                return; // success
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        // If the process is still alive after graceful, force-kill to clean up
        // and then fail the test.
        let _ = terminate(pid, TerminateMode::Force);
        let _ = child.wait();
        panic!("process {pid} still running after graceful terminate");
    }
}
