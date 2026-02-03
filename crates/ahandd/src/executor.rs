use std::sync::Arc;

use ahand_protocol::{envelope, job_event, Envelope, JobEvent, JobFinished, JobRequest};
use prost::Message;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::store::RunStore;

/// Runs a job and sends Envelope-wrapped events back via the channel.
///
/// Listens on `cancel_rx` for a cancellation signal.  When received the child
/// process is killed and a `JobFinished` with `error = "cancelled"` is sent.
///
/// If a `RunStore` is provided, stdout/stderr chunks and the final result are
/// persisted to disk.
pub async fn run_job(
    device_id: String,
    req: JobRequest,
    tx: mpsc::UnboundedSender<Vec<u8>>,
    mut cancel_rx: mpsc::Receiver<()>,
    store: Option<Arc<RunStore>>,
) {
    let job_id = req.job_id.clone();
    info!(job_id = %job_id, tool = %req.tool, "starting job");

    if let Some(s) = &store {
        s.start_run(&job_id, &req);
    }

    let mut cmd = Command::new(&req.tool);
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
            finish(&device_id, &job_id, -1, &e.to_string(), &tx, &store);
            return;
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
                        let _ = tx_out.send(encode_envelope(&envelope));
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
                        let _ = tx_err.send(encode_envelope(&envelope));
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
                        finish(&device_id, &job_id, -1, "timeout", &tx, &store);
                        return;
                    }
                }
            }
            _ = cancel_rx.recv() => {
                warn!(job_id = %job_id, "job cancelled, killing process");
                let _ = child.kill().await;
                let _ = stdout_handle.await;
                let _ = stderr_handle.await;
                finish(&device_id, &job_id, -1, "cancelled", &tx, &store);
                return;
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
                finish(&device_id, &job_id, -1, "cancelled", &tx, &store);
                return;
            }
        }
    };

    let _ = stdout_handle.await;
    let _ = stderr_handle.await;

    match wait_result {
        Some(Ok(status)) => {
            let code = status.code().unwrap_or(-1);
            info!(job_id = %job_id, exit_code = code, "job finished");
            finish(&device_id, &job_id, code, "", &tx, &store);
        }
        Some(Err(e)) => {
            warn!(job_id = %job_id, error = %e, "job wait error");
            finish(&device_id, &job_id, -1, &e.to_string(), &tx, &store);
        }
        None => {
            // Should not happen, but handle gracefully.
            finish(&device_id, &job_id, -1, "unknown error", &tx, &store);
        }
    }
}

fn finish(
    device_id: &str,
    job_id: &str,
    exit_code: i32,
    error: &str,
    tx: &mpsc::UnboundedSender<Vec<u8>>,
    store: &Option<Arc<RunStore>>,
) {
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
    let _ = tx.send(encode_envelope(&envelope));
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
            } else if let Some(data) = stderr_chunk {
                Some(job_event::Event::StderrChunk(data))
            } else {
                None
            },
        })),
        ..Default::default()
    }
}

fn encode_envelope(envelope: &Envelope) -> Vec<u8> {
    envelope.encode_to_vec()
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
