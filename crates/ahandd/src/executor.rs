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

    // Resolve `tool` — special "$SHELL" / "shell" tokens mean "the user's
    // default login shell" (read from the daemon process's $SHELL env, which
    // Tauri/launchctl populate from the user's session). The daemon then
    // execs that shell with `-l` (login mode) so `~/.bashrc`, `~/.zprofile`,
    // `~/.zshrc`, etc. are sourced — that's how `brew` / `nvm` / `pyenv` shims
    // get on PATH. Falls back to `/bin/sh` when $SHELL is unset.
    //
    // Any other `tool` value is treated as a literal executable path or PATH-
    // resolvable binary name (the previous behavior, untouched).
    let is_shell_sentinel = req.tool == "$SHELL" || req.tool == "shell";
    let tool_path = if is_shell_sentinel {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    } else {
        req.tool.clone()
    };

    let mut cmd = Command::new(&tool_path);
    if is_shell_sentinel {
        // Login mode: source the user's profile/rc files so the spawned
        // command sees the same PATH the user does in their terminal.
        cmd.arg("-l");
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
