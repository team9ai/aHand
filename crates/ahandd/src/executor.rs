use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ahand_protocol::{Envelope, JobEvent, JobFinished, JobRequest, envelope, job_event};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::agent::mcp_config::{
    MCP_CONFIG_ENV, MCP_CONFIG_MODE_ENV, McpConfig, McpConfigMode, server_names,
};
use crate::store::RunStore;

/// Messages that can be sent to the PTY stdin channel.
pub enum StdinInput {
    /// Raw bytes to write to the PTY.
    Data(Vec<u8>),
    /// Close the child stdin pipe when the caller reaches EOF.
    Close,
    /// Resize the PTY to the given dimensions.
    Resize { cols: u16, rows: u16 },
}

/// Sender half of the PTY stdin channel.
pub type StdinSender = mpsc::UnboundedSender<StdinInput>;

#[allow(clippy::result_unit_err)]
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
///
/// Exposed (`pub`) so the SDK ↔ daemon `tool` contract can be locked down
/// from an integration test (`tests/job_request_tool.rs`). The set of
/// accepted tool tokens is part of the public contract — bumping a string
/// here without updating the SDK side is exactly what the contract test
/// catches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTool {
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
///
/// `pub` for the same reason as [`ResolvedTool`]: this is the contract
/// surface validated by `tests/job_request_tool.rs`.
pub fn resolve_tool(tool: &str, shell_env: Option<&str>) -> ResolvedTool {
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

/// Runs a job with piped stdin/stdout/stderr and sends Envelope-wrapped events
/// back via the channel.
///
/// Unlike [`run_job_pty`], this does not allocate a TTY. It is intended for
/// streaming CLIs such as `claude -p --output-format stream-json` or
/// `codex exec`, where stdout/stderr should remain separately observable and
/// callers may still feed stdin bytes.
pub async fn run_job_stream<T>(
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
    info!(job_id = %job_id, tool = %req.tool, "starting stream job");
    let mut formatter = crate::result_parser::CodexFormatter::maybe_new(&req);

    if let Some(s) = &store {
        s.start_run(&job_id, &req);
    }

    let codex_home = match prepare_codex_mcp_home(&req, &job_id, &store) {
        Ok(home) => home,
        Err(error) => return finish(&device_id, &job_id, -1, &error, &tx, &store),
    };

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
        if k == MCP_CONFIG_ENV || k == MCP_CONFIG_MODE_ENV {
            continue;
        }
        cmd.env(k, v);
    }
    if let Some(home) = &codex_home {
        cmd.env("CODEX_HOME", home);
    }

    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let spawn_result = cmd.spawn();
    let mut child = match spawn_result {
        Ok(c) => c,
        Err(e) => {
            warn!(job_id = %job_id, error = %e, "failed to spawn stream job");
            cleanup_codex_home(codex_home.as_ref());
            return finish(&device_id, &job_id, -1, &e.to_string(), &tx, &store);
        }
    };

    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdin_handle = tokio::spawn(async move {
        let Some(mut stdin) = stdin else {
            return;
        };
        while let Some(input) = stdin_rx.recv().await {
            match input {
                StdinInput::Data(data) => {
                    if stdin.write_all(&data).await.is_err() {
                        break;
                    }
                    if stdin.flush().await.is_err() {
                        break;
                    }
                }
                StdinInput::Close => break,
                StdinInput::Resize { .. } => {
                    // Pipe-stream jobs intentionally have no terminal size.
                }
            }
        }
    });

    let tx_out = tx.clone();
    let tx_err = tx.clone();
    let device_id_out = device_id.clone();
    let device_id_err = device_id.clone();
    let job_id_out = job_id.clone();
    let job_id_err = job_id.clone();
    let store_out = store.clone();
    let store_err = store.clone();

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
                        if let Some(formatter) = &mut formatter {
                            for record in formatter.push_stdout(chunk) {
                                if let Some(s) = &store_out {
                                    s.append_observation(&job_id_out, &record);
                                }
                                if let Some(line) = observation_line(&record) {
                                    let envelope = make_event_envelope(
                                        &device_id_out,
                                        &job_id_out,
                                        Some(line),
                                        None,
                                    );
                                    let _ = tx_out.send(envelope);
                                }
                            }
                        } else {
                            let envelope = make_event_envelope(
                                &device_id_out,
                                &job_id_out,
                                Some(chunk.to_vec()),
                                None,
                            );
                            let _ = tx_out.send(envelope);
                        }
                    }
                    Err(_) => break,
                }
            }
            if let Some(formatter) = &mut formatter {
                for record in formatter.finish() {
                    if let Some(s) = &store_out {
                        s.append_observation(&job_id_out, &record);
                    }
                    if let Some(line) = observation_line(&record) {
                        let envelope =
                            make_event_envelope(&device_id_out, &job_id_out, Some(line), None);
                        let _ = tx_out.send(envelope);
                    }
                }
            }
        }
    });

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

    let wait_result = if req.timeout_ms > 0 {
        let timeout = std::time::Duration::from_millis(req.timeout_ms);
        tokio::select! {
            r = tokio::time::timeout(timeout, child.wait()) => {
                match r {
                    Ok(r) => Some(r),
                    Err(_) => {
                        warn!(job_id = %job_id, "stream job timed out, killing process");
                        let _ = child.kill().await;
                        stdin_handle.abort();
                        let _ = stdout_handle.await;
                        let _ = stderr_handle.await;
                        cleanup_codex_home(codex_home.as_ref());
                        return finish(&device_id, &job_id, -1, "timeout", &tx, &store);
                    }
                }
            }
            _ = cancel_rx.recv() => {
                warn!(job_id = %job_id, "stream job cancelled, killing process");
                let _ = child.kill().await;
                stdin_handle.abort();
                let _ = stdout_handle.await;
                let _ = stderr_handle.await;
                cleanup_codex_home(codex_home.as_ref());
                return finish(&device_id, &job_id, -1, "cancelled", &tx, &store);
            }
        }
    } else {
        tokio::select! {
            r = child.wait() => Some(r),
            _ = cancel_rx.recv() => {
                warn!(job_id = %job_id, "stream job cancelled, killing process");
                let _ = child.kill().await;
                stdin_handle.abort();
                let _ = stdout_handle.await;
                let _ = stderr_handle.await;
                cleanup_codex_home(codex_home.as_ref());
                return finish(&device_id, &job_id, -1, "cancelled", &tx, &store);
            }
        }
    };

    stdin_handle.abort();
    let _ = stdout_handle.await;
    let _ = stderr_handle.await;
    cleanup_codex_home(codex_home.as_ref());

    match wait_result {
        Some(Ok(status)) => {
            let code = status.code().unwrap_or(-1);
            info!(job_id = %job_id, exit_code = code, "stream job finished");
            finish(&device_id, &job_id, code, "", &tx, &store)
        }
        Some(Err(e)) => {
            warn!(job_id = %job_id, error = %e, "stream job wait error");
            finish(&device_id, &job_id, -1, &e.to_string(), &tx, &store)
        }
        None => finish(&device_id, &job_id, -1, "unknown error", &tx, &store),
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
    // INVARIANT: every spawn path must pipe `req.tool` through
    // `resolve_tool`, so the SDK's `$SHELL` / `shell` sentinels work the
    // same for `interactive=false` (run_job) and `interactive=true` (here).
    // Skipping this here would silently break interactive jobs with ENOENT.
    let resolved = resolve_tool(&req.tool, std::env::var("SHELL").ok().as_deref());

    let mut cmd = CommandBuilder::new(&resolved.path);
    for leading in &resolved.leading_args {
        cmd.arg(leading);
    }
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
                StdinInput::Close => break,
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

fn observation_line(record: &serde_json::Value) -> Option<Vec<u8>> {
    match serde_json::to_vec(record) {
        Ok(mut line) => {
            line.push(b'\n');
            Some(line)
        }
        Err(e) => {
            warn!(error = %e, "failed to encode observation stdout");
            None
        }
    }
}

fn prepare_codex_mcp_home(
    req: &JobRequest,
    job_id: &str,
    store: &Option<Arc<RunStore>>,
) -> Result<Option<PathBuf>, String> {
    if ahand_protocol::resolve_job_input_format(req) != ahand_protocol::INPUT_FORMAT_TEXT
        || ahand_protocol::resolve_job_output_format(req)
            != ahand_protocol::OUTPUT_FORMAT_CODEX_JSONL
    {
        return Ok(None);
    }
    let Some(mcp_config) = McpConfig::from_env(
        req.env.get(MCP_CONFIG_ENV).map(String::as_str),
        req.env.get(MCP_CONFIG_MODE_ENV).map(String::as_str),
    )?
    else {
        return Ok(None);
    };

    let home = std::env::temp_dir().join(format!(
        "ahand-codex-home-{}-{}-{}",
        std::process::id(),
        sanitize_filename(job_id),
        now_millis()
    ));
    std::fs::create_dir_all(&home)
        .map_err(|error| format!("failed to create Codex home {}: {error}", home.display()))?;

    let config_path = home.join("config.toml");
    let base_home = resolve_base_codex_home(req);
    if let Some(base_home) = &base_home {
        copy_codex_home_support_files(base_home, &home)?;
    }
    let mut config = read_base_codex_config(base_home.as_ref())?;
    merge_codex_mcp_config(&mut config, &mcp_config)?;
    std::fs::write(&config_path, config).map_err(|error| {
        format!(
            "failed to write Codex config {}: {error}",
            config_path.display()
        )
    })?;

    let names = server_names(&mcp_config.value);
    let server_count = names.len();
    append_json_artifact(
        store,
        job_id,
        "mcp.jsonl",
        &serde_json::json!({
            "kind": "mcp_config_injected",
            "agent": "codex",
            "mode": match mcp_config.mode {
                McpConfigMode::Merge => "merge",
                McpConfigMode::Replace => "replace",
            },
            "serverNames": names,
            "serverCount": server_count,
            "target": "CODEX_HOME/config.toml",
        }),
    );

    Ok(Some(home))
}

fn resolve_base_codex_home(req: &JobRequest) -> Option<PathBuf> {
    req.env
        .get("CODEX_HOME")
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("CODEX_HOME")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
}

fn read_base_codex_config(base_home: Option<&PathBuf>) -> Result<String, String> {
    let Some(base_home) = base_home else {
        return Ok(String::new());
    };
    let path = base_home.join("config.toml");
    if !path.exists() {
        return Ok(String::new());
    }
    std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "failed to read base Codex config {}: {error}",
            path.display()
        )
    })
}

fn copy_codex_home_support_files(base_home: &PathBuf, target_home: &PathBuf) -> Result<(), String> {
    for name in ["auth.json", "config.json", "instructions.md"] {
        let source = base_home.join(name);
        if source.is_file() {
            std::fs::copy(&source, target_home.join(name)).map_err(|error| {
                format!(
                    "failed to copy Codex home file {}: {error}",
                    source.display()
                )
            })?;
        }
    }
    let sessions = base_home.join("sessions");
    if sessions.exists() {
        let target = target_home.join("sessions");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&sessions, &target).map_err(|error| {
                format!(
                    "failed to link Codex sessions {} -> {}: {error}",
                    target.display(),
                    sessions.display()
                )
            })?;
        }
        #[cfg(not(unix))]
        {
            if sessions.is_dir() {
                std::fs::create_dir_all(&target).map_err(|error| {
                    format!(
                        "failed to create Codex sessions {}: {error}",
                        target.display()
                    )
                })?;
            }
        }
    }
    Ok(())
}

fn merge_codex_mcp_config(config: &mut String, mcp_config: &McpConfig) -> Result<(), String> {
    let mut value = if config.trim().is_empty() {
        toml::Value::Table(toml::map::Map::new())
    } else {
        config
            .parse::<toml::Value>()
            .map_err(|error| format!("failed to parse base Codex config.toml: {error}"))?
    };
    let root = value
        .as_table_mut()
        .ok_or_else(|| "Codex config.toml root must be a table".to_string())?;
    if matches!(mcp_config.mode, McpConfigMode::Replace) {
        root.remove("mcp_servers");
    }
    let servers = root
        .entry("mcp_servers".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| "Codex config.toml mcp_servers must be a table".to_string())?;

    let Some(json_servers) = mcp_config
        .value
        .get("mcpServers")
        .and_then(serde_json::Value::as_object)
    else {
        *config = toml::to_string_pretty(&value)
            .map_err(|error| format!("failed to serialize Codex config: {error}"))?;
        return Ok(());
    };

    for (name, server) in json_servers {
        let server = server
            .as_object()
            .ok_or_else(|| format!("mcp server {name:?} must be a JSON object"))?;
        let mut table = toml::map::Map::new();
        table.insert(
            "command".to_string(),
            toml::Value::String(
                server
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            ),
        );
        let args = server
            .get("args")
            .and_then(serde_json::Value::as_array)
            .map(|args| {
                args.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(|arg| toml::Value::String(arg.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        table.insert("args".to_string(), toml::Value::Array(args));
        if let Some(env) = server.get("env").and_then(serde_json::Value::as_object) {
            let mut env_table = toml::map::Map::new();
            for (key, value) in env {
                if let Some(value) = value.as_str() {
                    env_table.insert(key.clone(), toml::Value::String(value.to_string()));
                }
            }
            table.insert("env".to_string(), toml::Value::Table(env_table));
        }
        servers.insert(name.clone(), toml::Value::Table(table));
    }

    *config = toml::to_string_pretty(&value)
        .map_err(|error| format!("failed to serialize Codex config: {error}"))?;
    Ok(())
}

fn cleanup_codex_home(path: Option<&PathBuf>) {
    if let Some(path) = path {
        let _ = std::fs::remove_dir_all(path);
    }
}

fn append_json_artifact(
    store: &Option<Arc<RunStore>>,
    job_id: &str,
    name: &str,
    value: &serde_json::Value,
) {
    if let Some(store) = store
        && let Ok(mut line) = serde_json::to_vec(value)
    {
        line.push(b'\n');
        store.append_artifact(job_id, name, &line);
    }
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn sanitize_filename(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{StdinInput, merge_codex_mcp_config, run_job_stream};
    use crate::agent::mcp_config::{McpConfig, McpConfigMode};
    use ahand_protocol::{Envelope, ExecutionMode, JobRequest, envelope, job_event};
    use serde_json::json;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn stream_job_with_codex_format_emits_observations_on_stdout() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Envelope>();
        let (_cancel_tx, cancel_rx) = mpsc::channel::<()>(1);
        let (stdin_tx, stdin_rx) = mpsc::unbounded_channel::<StdinInput>();

        let raw_codex = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"thread-1\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"id\":\"item-1\",\"type\":\"agent_message\",\"text\":\"hello\"}}\n",
            "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}\n",
        );
        stdin_tx
            .send(StdinInput::Data(raw_codex.as_bytes().to_vec()))
            .unwrap();
        stdin_tx.send(StdinInput::Close).unwrap();

        let req = JobRequest {
            job_id: "job-format-codex".to_string(),
            tool: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "cat".to_string()],
            execution_mode: ExecutionMode::PipeStream as i32,
            result_parser: ahand_protocol::RESULT_PARSER_CODEX_JSONL.to_string(),
            format: ahand_protocol::FORMAT_CODEX.to_string(),
            ..Default::default()
        };

        let (exit_code, error) = run_job_stream(
            "device-1".to_string(),
            req,
            event_tx,
            cancel_rx,
            stdin_rx,
            None,
        )
        .await;
        assert_eq!(exit_code, 0);
        assert!(error.is_empty());

        let mut stdout_lines = Vec::new();
        let mut finished = false;
        while let Ok(envelope) = event_rx.try_recv() {
            match envelope.payload {
                Some(envelope::Payload::JobEvent(event)) => {
                    if let Some(job_event::Event::StdoutChunk(chunk)) = event.event {
                        let text = String::from_utf8(chunk).unwrap();
                        stdout_lines.extend(
                            text.lines()
                                .map(str::to_string)
                                .filter(|line| !line.is_empty()),
                        );
                    }
                }
                Some(envelope::Payload::JobFinished(done)) => {
                    assert_eq!(done.exit_code, 0);
                    finished = true;
                }
                _ => {}
            }
        }

        assert!(finished);
        assert_eq!(stdout_lines.len(), 4);
        let kinds = stdout_lines
            .iter()
            .map(|line| {
                let value: serde_json::Value = serde_json::from_str(line).unwrap();
                assert_ne!(value["type"].as_str(), Some("thread.started"));
                value["kind"].as_str().unwrap().to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                "agent_session",
                "llm_call_start",
                "llm_call_delta",
                "llm_call_end",
            ]
        );
    }

    #[test]
    fn codex_mcp_merge_overwrites_same_server_and_keeps_others() {
        let mut config = r#"
[mcp_servers.keep]
command = "uvx"
args = ["keep"]

[mcp_servers.fs]
command = "old"
"#
        .to_string();
        let mcp = McpConfig {
            mode: McpConfigMode::Merge,
            value: json!({
                "mcpServers": {
                    "fs": {
                        "command": "npx",
                        "args": ["-y", "server"],
                        "env": { "TOKEN": "secret" }
                    }
                }
            }),
        };

        merge_codex_mcp_config(&mut config, &mcp).unwrap();
        let value = config.parse::<toml::Value>().unwrap();
        assert_eq!(
            value["mcp_servers"]["keep"]["command"].as_str(),
            Some("uvx")
        );
        assert_eq!(value["mcp_servers"]["fs"]["command"].as_str(), Some("npx"));
        assert_eq!(
            value["mcp_servers"]["fs"]["env"]["TOKEN"].as_str(),
            Some("secret")
        );
    }

    #[test]
    fn codex_mcp_replace_removes_inherited_servers() {
        let mut config = r#"
[mcp_servers.keep]
command = "uvx"
"#
        .to_string();
        let mcp = McpConfig {
            mode: McpConfigMode::Replace,
            value: json!({
                "mcpServers": {
                    "fs": { "command": "npx" }
                }
            }),
        };

        merge_codex_mcp_config(&mut config, &mcp).unwrap();
        let value = config.parse::<toml::Value>().unwrap();
        assert!(value["mcp_servers"].get("keep").is_none());
        assert_eq!(value["mcp_servers"]["fs"]["command"].as_str(), Some("npx"));
    }

    #[tokio::test]
    async fn stream_job_without_format_emits_raw_stdout() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Envelope>();
        let (_cancel_tx, cancel_rx) = mpsc::channel::<()>(1);
        let (stdin_tx, stdin_rx) = mpsc::unbounded_channel::<StdinInput>();

        let raw_codex = "{\"type\":\"thread.started\",\"thread_id\":\"thread-1\"}\n";
        stdin_tx
            .send(StdinInput::Data(raw_codex.as_bytes().to_vec()))
            .unwrap();
        stdin_tx.send(StdinInput::Close).unwrap();

        let req = JobRequest {
            job_id: "job-format-raw".to_string(),
            tool: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "cat".to_string()],
            execution_mode: ExecutionMode::PipeStream as i32,
            result_parser: ahand_protocol::RESULT_PARSER_CODEX_JSONL.to_string(),
            format: ahand_protocol::FORMAT_RAW.to_string(),
            ..Default::default()
        };

        let (exit_code, error) = run_job_stream(
            "device-1".to_string(),
            req,
            event_tx,
            cancel_rx,
            stdin_rx,
            None,
        )
        .await;
        assert_eq!(exit_code, 0);
        assert!(error.is_empty());

        let mut stdout = String::new();
        while let Ok(envelope) = event_rx.try_recv() {
            if let Some(envelope::Payload::JobEvent(event)) = envelope.payload
                && let Some(job_event::Event::StdoutChunk(chunk)) = event.event
            {
                stdout.push_str(&String::from_utf8(chunk).unwrap());
            }
        }

        assert_eq!(stdout, raw_codex);
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
