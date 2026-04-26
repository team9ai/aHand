use std::io::{Read, Write};
use std::sync::Arc;

use ahand_protocol::{Envelope, JobEvent, JobFinished, JobRequest, envelope, job_event};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::store::RunStore;

/// Messages that can be sent to the PTY stdin channel.
pub enum StdinInput {
    /// Raw bytes to write to the PTY.
    Data(Vec<u8>),
    /// Resize the PTY to the given dimensions.
    Resize { cols: u16, rows: u16 },
}

/// Sender half of the PTY stdin channel.
pub type StdinSender = mpsc::UnboundedSender<StdinInput>;

pub trait EnvelopeSink: Clone + Send + Sync + 'static {
    fn send(&self, envelope: Envelope) -> Result<(), ()>;
}

impl EnvelopeSink for mpsc::UnboundedSender<Envelope> {
    fn send(&self, envelope: Envelope) -> Result<(), ()> {
        self.send(envelope).map_err(|_| ())
    }
}

/// Result of mapping a `JobRequest.tool` field to the actual binary the
/// daemon will exec. Pure data so the resolution rules can be unit-tested
/// without spawning real processes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedTool {
    /// Binary path/name passed to `Command::new`.
    pub path: String,
    /// Args inserted before `req.args` (e.g. `-l` for login-shell mode).
    pub leading_args: Vec<String>,
}

/// Resolve `tool` against the daemon's environment.
///
/// Special tokens `"$SHELL"` and `"shell"` mean "the user's default login
/// shell" — caller passes `std::env::var("SHELL").ok().as_deref()` for
/// `shell_env`. Resolution falls back to `/bin/sh` when `$SHELL` is unset.
/// Sentinel mode also prepends `-l` so the spawned shell sources the user's
/// profile / rc files (gives spawned commands the same PATH the user sees
/// in their terminal — brew, nvm, pyenv shims, etc.).
///
/// Any other `tool` value is treated as a literal executable path or
/// PATH-resolvable binary name; no leading args.
pub(crate) fn resolve_tool(tool: &str, shell_env: Option<&str>) -> ResolvedTool {
    if tool == "$SHELL" || tool == "shell" {
        ResolvedTool {
            path: shell_env.unwrap_or("/bin/sh").to_string(),
            leading_args: vec!["-l".to_string()],
        }
    } else {
        ResolvedTool {
            path: tool.to_string(),
            leading_args: Vec::new(),
        }
    }
}

/// Runs a job and sends Envelope-wrapped events back via the channel.
///
/// Listens on `cancel_rx` for a cancellation signal.  When received the child
/// process is killed and a `JobFinished` with `error = "cancelled"` is sent.
///
/// If a `RunStore` is provided, stdout/stderr chunks and the final result are
/// persisted to disk.
/// Returns `(exit_code, error)` for the caller to use (e.g. for idempotency caching).
pub async fn run_job<T>(
    device_id: String,
    req: JobRequest,
    tx: T,
    mut cancel_rx: mpsc::Receiver<()>,
    store: Option<Arc<RunStore>>,
) -> (i32, String)
where
    T: EnvelopeSink,
{
    let job_id = req.job_id.clone();
    info!(job_id = %job_id, tool = %req.tool, "starting job");

    if let Some(s) = &store {
        s.start_run(&job_id, &req);
    }

    let resolved = resolve_tool(&req.tool, std::env::var("SHELL").ok().as_deref());

    let mut cmd = Command::new(&resolved.path);
    for leading in &resolved.leading_args {
        cmd.arg(leading);
    }
    cmd.args(&req.args);

    if !req.cwd.is_empty() {
        cmd.current_dir(&req.cwd);
    }

    for (k, v) in &req.env {
        cmd.env(k, v);
    }

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let spawn_result = cmd.spawn();
    let mut child = match spawn_result {
        Ok(c) => c,
        Err(e) => {
            warn!(job_id = %job_id, error = %e, "failed to spawn");
            return finish(&device_id, &job_id, -1, &e.to_string(), &tx, &store);
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let tx_out = tx.clone();
    let tx_err = tx.clone();
    let device_id_out = device_id.clone();
    let device_id_err = device_id.clone();
    let job_id_out = job_id.clone();
    let job_id_err = job_id.clone();
    let store_out = store.clone();
    let store_err = store.clone();

    // Stream stdout.
    let stdout_handle = tokio::spawn(async move {
        if let Some(mut out) = stdout {
            let mut buf = vec![0u8; 4096];
            loop {
                match out.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = &buf[..n];
                        if let Some(s) = &store_out {
                            s.append_stdout(&job_id_out, chunk);
                        }
                        let envelope = make_event_envelope(
                            &device_id_out,
                            &job_id_out,
                            Some(chunk.to_vec()),
                            None,
                        );
                        let _ = tx_out.send(envelope);
                    }
                    Err(_) => break,
                }
            }
        }
    });

    // Stream stderr.
    let stderr_handle = tokio::spawn(async move {
        if let Some(mut err) = stderr {
            let mut buf = vec![0u8; 4096];
            loop {
                match err.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = &buf[..n];
                        if let Some(s) = &store_err {
                            s.append_stderr(&job_id_err, chunk);
                        }
                        let envelope = make_event_envelope(
                            &device_id_err,
                            &job_id_err,
                            None,
                            Some(chunk.to_vec()),
                        );
                        let _ = tx_err.send(envelope);
                    }
                    Err(_) => break,
                }
            }
        }
    });

    // Wait for the child, with optional timeout and cancel support.
    let wait_result = if req.timeout_ms > 0 {
        let timeout = std::time::Duration::from_millis(req.timeout_ms);
        tokio::select! {
            r = tokio::time::timeout(timeout, child.wait()) => {
                match r {
                    Ok(r) => Some(r),
                    Err(_) => {
                        warn!(job_id = %job_id, "job timed out, killing process");
                        let _ = child.kill().await;
                        let _ = stdout_handle.await;
                        let _ = stderr_handle.await;
                        return finish(&device_id, &job_id, -1, "timeout", &tx, &store);
                    }
                }
            }
            _ = cancel_rx.recv() => {
                warn!(job_id = %job_id, "job cancelled, killing process");
                let _ = child.kill().await;
                let _ = stdout_handle.await;
                let _ = stderr_handle.await;
                return finish(&device_id, &job_id, -1, "cancelled", &tx, &store);
            }
        }
    } else {
        tokio::select! {
            r = child.wait() => Some(r),
            _ = cancel_rx.recv() => {
                warn!(job_id = %job_id, "job cancelled, killing process");
                let _ = child.kill().await;
                let _ = stdout_handle.await;
                let _ = stderr_handle.await;
                return finish(&device_id, &job_id, -1, "cancelled", &tx, &store);
            }
        }
    };

    let _ = stdout_handle.await;
    let _ = stderr_handle.await;

    match wait_result {
        Some(Ok(status)) => {
            let code = status.code().unwrap_or(-1);
            info!(job_id = %job_id, exit_code = code, "job finished");
            finish(&device_id, &job_id, code, "", &tx, &store)
        }
        Some(Err(e)) => {
            warn!(job_id = %job_id, error = %e, "job wait error");
            finish(&device_id, &job_id, -1, &e.to_string(), &tx, &store)
        }
        None => {
            // Should not happen, but handle gracefully.
            finish(&device_id, &job_id, -1, "unknown error", &tx, &store)
        }
    }
}

/// Runs a job inside a PTY and sends Envelope-wrapped events back via the channel.
///
/// Similar to [`run_job`] but allocates a pseudo-terminal so that the child
/// process sees a TTY (useful for interactive tools).  PTY merges stdout and
/// stderr into a single output stream.
///
/// `stdin_rx` receives [`StdinInput`] messages: raw data to forward to the
/// child, or resize requests.
///
/// Returns `(exit_code, error)` just like `run_job`.
pub async fn run_job_pty<T>(
    device_id: String,
    req: JobRequest,
    tx: T,
    mut cancel_rx: mpsc::Receiver<()>,
    mut stdin_rx: mpsc::UnboundedReceiver<StdinInput>,
    store: Option<Arc<RunStore>>,
) -> (i32, String)
where
    T: EnvelopeSink,
{
    let job_id = req.job_id.clone();
    info!(job_id = %job_id, tool = %req.tool, "starting pty job");

    if let Some(s) = &store {
        s.start_run(&job_id, &req);
    }

    // --- Allocate PTY ---------------------------------------------------
    let pty_system = native_pty_system();
    let pair = match pty_system.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => {
            warn!(job_id = %job_id, error = %e, "failed to open pty");
            return finish(&device_id, &job_id, -1, &e.to_string(), &tx, &store);
        }
    };

    // --- Build command --------------------------------------------------
    let mut cmd = CommandBuilder::new(&req.tool);
    cmd.args(&req.args);

    if !req.cwd.is_empty() {
        cmd.cwd(&req.cwd);
    }

    for (k, v) in &req.env {
        cmd.env(k, v);
    }

    // --- Spawn child in the PTY ----------------------------------------
    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            warn!(job_id = %job_id, error = %e, "failed to spawn in pty");
            return finish(&device_id, &job_id, -1, &e.to_string(), &tx, &store);
        }
    };

    // Drop the slave so the reader gets EOF when the child exits.
    drop(pair.slave);

    // --- I/O handles from master ----------------------------------------
    let reader = match pair.master.try_clone_reader() {
        Ok(r) => r,
        Err(e) => {
            warn!(job_id = %job_id, error = %e, "failed to clone pty reader");
            let _ = child.kill();
            return finish(&device_id, &job_id, -1, &e.to_string(), &tx, &store);
        }
    };

    let writer = match pair.master.take_writer() {
        Ok(w) => w,
        Err(e) => {
            warn!(job_id = %job_id, error = %e, "failed to take pty writer");
            let _ = child.kill();
            return finish(&device_id, &job_id, -1, &e.to_string(), &tx, &store);
        }
    };

    // --- Output reader (blocking → spawn_blocking) ----------------------
    let tx_out = tx.clone();
    let device_id_out = device_id.clone();
    let job_id_out = job_id.clone();
    let store_out = store.clone();

    let output_handle = tokio::task::spawn_blocking(move || {
        let mut reader = reader;
        let mut buf = vec![0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    if let Some(s) = &store_out {
                        s.append_stdout(&job_id_out, chunk);
                    }
                    let envelope = make_event_envelope(
                        &device_id_out,
                        &job_id_out,
                        Some(chunk.to_vec()),
                        None,
                    );
                    let _ = tx_out.send(envelope);
                }
                Err(_) => break,
            }
        }
    });

    // --- Stdin writer (async task) --------------------------------------
    // We need the master handle for resize, so we share it via Arc<Mutex>.
    // The writer is moved into the stdin task.
    let master = Arc::new(std::sync::Mutex::new(pair.master));
    let master_stdin = Arc::clone(&master);

    let stdin_handle = tokio::spawn(async move {
        let mut writer = writer;
        while let Some(input) = stdin_rx.recv().await {
            match input {
                StdinInput::Data(data) => {
                    // Write is blocking but should be fast for stdin.
                    if writer.write_all(&data).is_err() {
                        break;
                    }
                    if writer.flush().is_err() {
                        break;
                    }
                }
                StdinInput::Resize { cols, rows } => {
                    if let Ok(m) = master_stdin.lock() {
                        let _ = m.resize(PtySize {
                            rows,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                }
            }
        }
    });

    // --- Wait for child with timeout/cancel support ---------------------
    let job_id_wait = job_id.clone();
    let wait_future = tokio::task::spawn_blocking(move || child.wait());

    let wait_result = if req.timeout_ms > 0 {
        let timeout = std::time::Duration::from_millis(req.timeout_ms);
        tokio::select! {
            r = tokio::time::timeout(timeout, wait_future) => {
                match r {
                    Ok(join_result) => Some(join_result),
                    Err(_) => {
                        warn!(job_id = %job_id_wait, "pty job timed out");
                        // Drop the master to signal EOF and potentially unblock the reader.
                        drop(master);
                        stdin_handle.abort();
                        let _ = output_handle.await;
                        return finish(&device_id, &job_id, -1, "timeout", &tx, &store);
                    }
                }
            }
            _ = cancel_rx.recv() => {
                warn!(job_id = %job_id_wait, "pty job cancelled");
                drop(master);
                stdin_handle.abort();
                let _ = output_handle.await;
                return finish(&device_id, &job_id, -1, "cancelled", &tx, &store);
            }
        }
    } else {
        tokio::select! {
            r = wait_future => Some(r),
            _ = cancel_rx.recv() => {
                warn!(job_id = %job_id_wait, "pty job cancelled");
                drop(master);
                stdin_handle.abort();
                let _ = output_handle.await;
                return finish(&device_id, &job_id, -1, "cancelled", &tx, &store);
            }
        }
    };

    // Child has exited — clean up I/O tasks.
    // Drop master/writer to signal EOF to the reader.
    drop(master);
    stdin_handle.abort();
    let _ = output_handle.await;

    match wait_result {
        Some(Ok(Ok(status))) => {
            let code = status.exit_code() as i32;
            info!(job_id = %job_id, exit_code = code, "pty job finished");
            finish(&device_id, &job_id, code, "", &tx, &store)
        }
        Some(Ok(Err(e))) => {
            warn!(job_id = %job_id, error = %e, "pty job wait error");
            finish(&device_id, &job_id, -1, &e.to_string(), &tx, &store)
        }
        Some(Err(e)) => {
            // JoinError from spawn_blocking
            warn!(job_id = %job_id, error = %e, "pty job join error");
            finish(&device_id, &job_id, -1, &e.to_string(), &tx, &store)
        }
        None => finish(&device_id, &job_id, -1, "unknown error", &tx, &store),
    }
}

fn finish(
    device_id: &str,
    job_id: &str,
    exit_code: i32,
    error: &str,
    tx: &impl EnvelopeSink,
    store: &Option<Arc<RunStore>>,
) -> (i32, String) {
    if let Some(s) = &store {
        s.finish_run(job_id, exit_code, error);
    }

    let envelope = Envelope {
        device_id: device_id.to_string(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::JobFinished(JobFinished {
            job_id: job_id.to_string(),
            exit_code,
            error: error.to_string(),
        })),
        ..Default::default()
    };
    let _ = tx.send(envelope);
    (exit_code, error.to_string())
}

fn make_event_envelope(
    device_id: &str,
    job_id: &str,
    stdout_chunk: Option<Vec<u8>>,
    stderr_chunk: Option<Vec<u8>>,
) -> Envelope {
    Envelope {
        device_id: device_id.to_string(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::JobEvent(JobEvent {
            job_id: job_id.to_string(),
            event: if let Some(data) = stdout_chunk {
                Some(job_event::Event::StdoutChunk(data))
            } else {
                stderr_chunk.map(job_event::Event::StderrChunk)
            },
        })),
        ..Default::default()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn new_msg_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("d-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tool_resolution_tests {
    use super::{ResolvedTool, resolve_tool};

    #[test]
    fn dollar_shell_sentinel_resolves_to_shell_env_with_login_flag() {
        let r = resolve_tool("$SHELL", Some("/bin/zsh"));
        assert_eq!(
            r,
            ResolvedTool {
                path: "/bin/zsh".to_string(),
                leading_args: vec!["-l".to_string()],
            }
        );
    }

    #[test]
    fn shell_sentinel_resolves_to_shell_env_with_login_flag() {
        // The bare `"shell"` form (without `$`) is also accepted for
        // ergonomics; older callers may emit either.
        let r = resolve_tool("shell", Some("/bin/bash"));
        assert_eq!(
            r,
            ResolvedTool {
                path: "/bin/bash".to_string(),
                leading_args: vec!["-l".to_string()],
            }
        );
    }

    #[test]
    fn shell_sentinel_falls_back_to_bin_sh_when_shell_env_is_unset() {
        // Tauri/launchctl normally propagate $SHELL from the user's
        // session, but if for some reason it isn't set the daemon must
        // still spawn _something_ — `/bin/sh` is POSIX-mandated, so
        // it's the safe fallback rather than failing with ENOENT.
        let r = resolve_tool("$SHELL", None);
        assert_eq!(
            r,
            ResolvedTool {
                path: "/bin/sh".to_string(),
                leading_args: vec!["-l".to_string()],
            }
        );
    }

    #[test]
    fn explicit_absolute_binary_path_is_passed_through_unchanged() {
        let r = resolve_tool("/usr/bin/whoami", Some("/bin/zsh"));
        assert_eq!(
            r,
            ResolvedTool {
                path: "/usr/bin/whoami".to_string(),
                leading_args: vec![],
            }
        );
    }

    #[test]
    fn explicit_path_resolvable_binary_name_is_passed_through_unchanged() {
        let r = resolve_tool("git", Some("/bin/zsh"));
        assert_eq!(
            r,
            ResolvedTool {
                path: "git".to_string(),
                leading_args: vec![],
            }
        );
    }

    #[test]
    fn explicit_bin_sh_is_a_literal_not_a_sentinel() {
        // `/bin/sh` should NOT be treated as the "shell" sentinel — this
        // matters because callers (claw-hive ahand integration) used to
        // hardcode `/bin/sh` and we want them to still produce a literal
        // pass-through if they ever revert. The sentinel is only the
        // bare strings `"shell"` / `"$SHELL"`.
        let r = resolve_tool("/bin/sh", Some("/bin/zsh"));
        assert_eq!(
            r,
            ResolvedTool {
                path: "/bin/sh".to_string(),
                leading_args: vec![],
            }
        );
    }

    #[test]
    fn empty_tool_is_passed_through_as_empty_path_no_leading_args() {
        // Garbage-in/garbage-out: validation lives at the wire layer
        // (protocol DTOs reject empty tool); resolve_tool itself does not
        // own that check, just the sentinel mapping.
        let r = resolve_tool("", Some("/bin/zsh"));
        assert_eq!(
            r,
            ResolvedTool {
                path: String::new(),
                leading_args: vec![],
            }
        );
    }
}
