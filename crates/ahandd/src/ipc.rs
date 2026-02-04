use std::path::{Path, PathBuf};
use std::sync::Arc;

use ahand_protocol::{envelope, Envelope, JobFinished, JobRejected};
use prost::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

use crate::approval::ApprovalManager;
use crate::config::Config;
use crate::executor;
use crate::policy::{PolicyChecker, PolicyDecision};
use crate::registry::{IsKnown, JobRegistry};
use crate::store::RunStore;

/// Start the IPC server on the given Unix socket path.
#[allow(clippy::too_many_arguments)]
pub async fn serve_ipc(
    socket_path: PathBuf,
    socket_mode: u32,
    registry: Arc<JobRegistry>,
    store: Option<Arc<RunStore>>,
    policy: Arc<PolicyChecker>,
    approval_mgr: Arc<ApprovalManager>,
    approval_broadcast_tx: broadcast::Sender<Envelope>,
    device_id: String,
    config_path: Option<PathBuf>,
) -> anyhow::Result<()> {
    // Remove stale socket file if it exists.
    let _ = std::fs::remove_file(&socket_path);

    // Ensure parent directory exists.
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&socket_path)?;

    // Set socket permissions.
    set_permissions(&socket_path, socket_mode)?;

    info!(path = %socket_path.display(), mode = format!("{:04o}", socket_mode), "IPC server listening");

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                // Get peer credentials before splitting the stream.
                let caller_uid = match stream.peer_cred() {
                    Ok(cred) => format!("uid:{}", cred.uid()),
                    Err(e) => {
                        warn!(error = %e, "IPC: failed to get peer credentials");
                        "uid:unknown".to_string()
                    }
                };

                let reg = Arc::clone(&registry);
                let st = store.clone();
                let pol = Arc::clone(&policy);
                let amgr = Arc::clone(&approval_mgr);
                let bcast = approval_broadcast_tx.clone();
                let did = device_id.clone();
                let cfgp = config_path.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_ipc_conn(
                        stream, reg, st, pol, amgr, bcast, did, caller_uid, cfgp,
                    )
                    .await
                    {
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

#[allow(clippy::too_many_arguments)]
async fn handle_ipc_conn(
    stream: UnixStream,
    registry: Arc<JobRegistry>,
    store: Option<Arc<RunStore>>,
    policy: Arc<PolicyChecker>,
    approval_mgr: Arc<ApprovalManager>,
    approval_broadcast_tx: broadcast::Sender<Envelope>,
    device_id: String,
    caller_uid: String,
    config_path: Option<PathBuf>,
) -> anyhow::Result<()> {
    let (reader, writer) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(reader);

    info!(caller_uid = %caller_uid, "IPC: new connection");

    // Channel for sending responses back through the IPC stream.
    let (tx, mut rx) = mpsc::unbounded_channel::<Envelope>();

    // Subscribe to the approval broadcast channel.
    let mut approval_rx = approval_broadcast_tx.subscribe();

    // Task: forward outgoing envelopes and broadcast approval requests to the IPC stream.
    let send_handle = tokio::spawn(async move {
        let mut writer = writer;
        loop {
            tokio::select! {
                msg = rx.recv() => {
                    match msg {
                        Some(envelope) => {
                            let data = envelope.encode_to_vec();
                            if write_frame(&mut writer, &data).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                bcast = approval_rx.recv() => {
                    match bcast {
                        Ok(envelope) => {
                            let data = envelope.encode_to_vec();
                            if write_frame(&mut writer, &data).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(missed = n, "IPC: broadcast lagged, missed messages");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
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

                // Three-way policy check.
                match policy.check(&req, &caller_uid).await {
                    PolicyDecision::Deny(reason) => {
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
                    }
                    PolicyDecision::Allow => {
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
                    PolicyDecision::NeedsApproval { reason, detected_domains } => {
                        info!(job_id = %req.job_id, reason = %reason, "IPC: job needs approval");

                        let (approval_req, approval_rx) = approval_mgr
                            .submit(req.clone(), &caller_uid, reason, detected_domains)
                            .await;

                        // Send ApprovalRequest to this IPC client.
                        let approval_env = Envelope {
                            device_id: device_id.clone(),
                            msg_id: new_msg_id(),
                            ts_ms: now_ms(),
                            payload: Some(envelope::Payload::ApprovalRequest(
                                approval_req.clone(),
                            )),
                            ..Default::default()
                        };
                        let _ = tx.send(approval_env.clone());

                        // Also broadcast to other IPC clients (the broadcast channel
                        // is also received by the WS client for cloud notification).
                        let _ = approval_broadcast_tx.send(approval_env);

                        // Spawn a task to wait for approval.
                        let tx_clone = tx.clone();
                        let did = device_id.clone();
                        let reg = Arc::clone(&registry);
                        let st = store.clone();
                        let amgr = Arc::clone(&approval_mgr);
                        let pol = Arc::clone(&policy);
                        let timeout = amgr.default_timeout();
                        let job_id = req.job_id.clone();
                        let cuid = caller_uid.clone();

                        tokio::spawn(async move {
                            let result = tokio::time::timeout(timeout, approval_rx).await;
                            match result {
                                Ok(Ok(resp)) if resp.approved => {
                                    info!(job_id = %job_id, "IPC: approval granted");
                                    if resp.remember {
                                        pol.remember_approval(
                                            &cuid,
                                            &req.tool,
                                            &approval_req.detected_domains,
                                        )
                                        .await;
                                    }
                                    let (cancel_tx, cancel_rx) = mpsc::channel(1);
                                    reg.register(job_id.clone(), cancel_tx).await;
                                    let _permit = reg.acquire_permit().await;
                                    let (exit_code, error) =
                                        executor::run_job(did, req, tx_clone, cancel_rx, st).await;
                                    reg.remove(&job_id).await;
                                    reg.mark_completed(job_id, exit_code, error).await;
                                }
                                _ => {
                                    info!(job_id = %job_id, "IPC: approval denied or timed out");
                                    amgr.expire(&job_id).await;
                                    let reject_env = Envelope {
                                        device_id: did,
                                        msg_id: new_msg_id(),
                                        ts_ms: now_ms(),
                                        payload: Some(envelope::Payload::JobRejected(
                                            JobRejected {
                                                job_id,
                                                reason: "approval denied or timed out".to_string(),
                                            },
                                        )),
                                        ..Default::default()
                                    };
                                    let _ = tx_clone.send(reject_env);
                                }
                            }
                        });
                    }
                }
            }
            Some(envelope::Payload::CancelJob(cancel)) => {
                info!(job_id = %cancel.job_id, "IPC: received cancel request");
                registry.cancel(&cancel.job_id).await;
            }
            Some(envelope::Payload::ApprovalResponse(resp)) => {
                info!(job_id = %resp.job_id, approved = resp.approved, "IPC: received approval response");
                approval_mgr.resolve(&resp).await;
            }
            Some(envelope::Payload::PolicyQuery(_)) => {
                info!("IPC: received policy query");
                let state = policy.get_state().await;
                let state_env = Envelope {
                    device_id: device_id.clone(),
                    msg_id: new_msg_id(),
                    ts_ms: now_ms(),
                    payload: Some(envelope::Payload::PolicyState(state)),
                    ..Default::default()
                };
                let _ = tx.send(state_env);
            }
            Some(envelope::Payload::PolicyUpdate(update)) => {
                info!("IPC: received policy update");
                policy.apply_update(&update).await;

                // Persist to config file if available.
                if let Some(path) = &config_path
                    && let Ok(mut existing) = Config::load(path)
                {
                    existing.policy = policy.config_snapshot().await;
                    if let Err(e) = existing.save(path) {
                        warn!(error = %e, "IPC: failed to persist policy update to config file");
                    }
                }

                let state = policy.get_state().await;
                let state_env = Envelope {
                    device_id: device_id.clone(),
                    msg_id: new_msg_id(),
                    ts_ms: now_ms(),
                    payload: Some(envelope::Payload::PolicyState(state)),
                    ..Default::default()
                };
                let _ = tx.send(state_env);
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

fn set_permissions(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(mode);
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
