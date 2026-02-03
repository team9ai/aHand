use std::path::{Path, PathBuf};
use std::sync::Arc;

use ahand_protocol::{envelope, Envelope, JobFinished, JobRejected};
use prost::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::executor;
use crate::policy::PolicyChecker;
use crate::registry::{IsKnown, JobRegistry};
use crate::store::RunStore;

/// Start the IPC server on the given Unix socket path.
pub async fn serve_ipc(
    socket_path: PathBuf,
    registry: Arc<JobRegistry>,
    store: Option<Arc<RunStore>>,
    policy: Arc<PolicyChecker>,
    device_id: String,
) -> anyhow::Result<()> {
    // Remove stale socket file if it exists.
    let _ = std::fs::remove_file(&socket_path);

    // Ensure parent directory exists.
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&socket_path)?;

    // Set socket permissions to 0600 (owner-only).
    set_permissions_0600(&socket_path)?;

    info!(path = %socket_path.display(), "IPC server listening");

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let reg = Arc::clone(&registry);
                let st = store.clone();
                let pol = Arc::clone(&policy);
                let did = device_id.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_ipc_conn(stream, reg, st, pol, did).await {
                        warn!(error = %e, "IPC connection error");
                    }
                });
            }
            Err(e) => {
                error!(error = %e, "IPC accept error");
            }
        }
    }
}

async fn handle_ipc_conn(
    stream: UnixStream,
    registry: Arc<JobRegistry>,
    store: Option<Arc<RunStore>>,
    policy: Arc<PolicyChecker>,
    device_id: String,
) -> anyhow::Result<()> {
    let (reader, writer) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(reader);

    // Channel for sending responses back through the IPC stream.
    let (tx, mut rx) = mpsc::unbounded_channel::<Envelope>();

    // Task: forward outgoing envelopes to the IPC stream as frames.
    let send_handle = tokio::spawn(async move {
        let mut writer = writer;
        while let Some(envelope) = rx.recv().await {
            let data = envelope.encode_to_vec();
            if write_frame(&mut writer, &data).await.is_err() {
                break;
            }
        }
    });

    // Read frames from the IPC stream.
    loop {
        let data = match read_frame(&mut reader).await {
            Ok(d) => d,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    // Client disconnected.
                    break;
                }
                warn!(error = %e, "IPC read error");
                break;
            }
        };

        let envelope = match Envelope::decode(data.as_slice()) {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "IPC: failed to decode envelope");
                continue;
            }
        };

        match envelope.payload {
            Some(envelope::Payload::JobRequest(req)) => {
                // Idempotency check.
                match registry.is_known(&req.job_id).await {
                    IsKnown::Running => {
                        warn!(job_id = %req.job_id, "IPC: duplicate job_id, already running");
                        continue;
                    }
                    IsKnown::Completed(c) => {
                        info!(job_id = %req.job_id, "IPC: duplicate job_id, returning cached result");
                        let finished_env = Envelope {
                            device_id: device_id.clone(),
                            msg_id: new_msg_id(),
                            ts_ms: now_ms(),
                            payload: Some(envelope::Payload::JobFinished(JobFinished {
                                job_id: req.job_id.clone(),
                                exit_code: c.exit_code,
                                error: c.error,
                            })),
                            ..Default::default()
                        };
                        let _ = tx.send(finished_env);
                        continue;
                    }
                    IsKnown::Unknown => {}
                }

                // Policy check.
                if let Err(reason) = policy.check(&req) {
                    warn!(job_id = %req.job_id, reason = %reason, "IPC: job rejected by policy");
                    let reject_env = Envelope {
                        device_id: device_id.clone(),
                        msg_id: new_msg_id(),
                        ts_ms: now_ms(),
                        payload: Some(envelope::Payload::JobRejected(JobRejected {
                            job_id: req.job_id.clone(),
                            reason,
                        })),
                        ..Default::default()
                    };
                    let _ = tx.send(reject_env);
                } else {
                    let job_id = req.job_id.clone();
                    let tx_clone = tx.clone();
                    let did = device_id.clone();
                    let reg = Arc::clone(&registry);
                    let st = store.clone();

                    let (cancel_tx, cancel_rx) = mpsc::channel(1);
                    reg.register(job_id.clone(), cancel_tx).await;

                    let active = reg.active_count().await;
                    info!(job_id = %job_id, active_jobs = active, "IPC: job accepted");

                    tokio::spawn(async move {
                        let _permit = reg.acquire_permit().await;
                        let (exit_code, error) =
                            executor::run_job(did, req, tx_clone, cancel_rx, st).await;
                        reg.remove(&job_id).await;
                        reg.mark_completed(job_id, exit_code, error).await;
                    });
                }
            }
            Some(envelope::Payload::CancelJob(cancel)) => {
                info!(job_id = %cancel.job_id, "IPC: received cancel request");
                registry.cancel(&cancel.job_id).await;
            }
            _ => {}
        }
    }

    drop(tx);
    let _ = send_handle.await;
    Ok(())
}

/// Read a length-prefixed frame: [4 bytes big-endian u32 length][N bytes payload].
async fn read_frame<R: AsyncReadExt + Unpin>(reader: &mut R) -> std::io::Result<Vec<u8>> {
    let len = reader.read_u32().await? as usize;
    if len > 16 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Write a length-prefixed frame.
async fn write_frame<W: AsyncWriteExt + Unpin>(writer: &mut W, data: &[u8]) -> std::io::Result<()> {
    writer.write_u32(data.len() as u32).await?;
    writer.write_all(data).await?;
    writer.flush().await?;
    Ok(())
}

fn set_permissions_0600(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms)
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
    format!("ipc-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}
