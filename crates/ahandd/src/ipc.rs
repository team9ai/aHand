use std::sync::Arc;

use ahand_platform::ipc::{IpcEndpoint, IpcListener};
use ahand_protocol::{BrowserResponse, Envelope, JobFinished, JobRejected, SessionMode, envelope};
use prost::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

use crate::approval::ApprovalManager;
use crate::browser::BrowserManager;
use crate::executor;
use crate::file_manager::FileManager;
use crate::plugin_runtime::{CapabilityKind, CapabilityUnavailable, JobProvider};
use crate::registry::{IsKnown, JobRegistry};
use crate::session::{SessionDecision, SessionManager};
use crate::store::RunStore;

/// Start the IPC server on the given endpoint.
#[allow(clippy::too_many_arguments)]
pub async fn serve_ipc(
    endpoint: IpcEndpoint,
    socket_mode: u32,
    registry: Arc<JobRegistry>,
    store: Option<Arc<RunStore>>,
    session_mgr: Arc<SessionManager>,
    approval_mgr: Arc<ApprovalManager>,
    approval_broadcast_tx: broadcast::Sender<Envelope>,
    device_id: String,
    browser_mgr: Arc<BrowserManager>,
    file_mgr: Arc<FileManager>,
) -> anyhow::Result<()> {
    let mut listener = IpcListener::bind(&endpoint, socket_mode)?;
    info!(endpoint = %endpoint.as_path().display(), "IPC server listening");

    loop {
        match listener.accept().await {
            Ok((stream, caller_id)) => {
                let reg = Arc::clone(&registry);
                let st = store.clone();
                let smgr = Arc::clone(&session_mgr);
                let amgr = Arc::clone(&approval_mgr);
                let bcast = approval_broadcast_tx.clone();
                let did = device_id.clone();
                let bmgr = Arc::clone(&browser_mgr);
                let fmgr = Arc::clone(&file_mgr);
                tokio::spawn(async move {
                    if let Err(e) = handle_ipc_conn(
                        stream, reg, st, smgr, amgr, bcast, did, caller_id, bmgr, fmgr,
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
async fn handle_ipc_conn<S>(
    stream: S,
    registry: Arc<JobRegistry>,
    store: Option<Arc<RunStore>>,
    session_mgr: Arc<SessionManager>,
    approval_mgr: Arc<ApprovalManager>,
    approval_broadcast_tx: broadcast::Sender<Envelope>,
    device_id: String,
    caller_id: String,
    browser_mgr: Arc<BrowserManager>,
    file_mgr: Arc<FileManager>,
) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (reader, writer) = tokio::io::split(stream);
    let mut reader = tokio::io::BufReader::new(reader);

    info!(caller_id = %caller_id, "IPC: new connection");

    // Register the IPC caller so session queries return it.
    session_mgr.register_caller(&caller_id).await;

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
                let provider_registry = match crate::plugin_runtime::build_provider_registry(
                    &browser_mgr,
                    &file_mgr,
                )
                .await
                {
                    Ok(registry) => registry,
                    Err(err) => {
                        warn!(
                            job_id = %req.job_id,
                            tool = %req.tool,
                            error = %err,
                            "IPC: job rejected because host resources could not be inspected"
                        );
                        let reason = format!(
                            "exec capability unavailable: failed to inspect host resources: {err}"
                        );
                        let _ =
                            tx.send(job_capability_rejection_envelope(&device_id, &req, reason));
                        continue;
                    }
                };
                let job_provider = match provider_registry.resolve_job_provider(&req.tool) {
                    Ok(provider) => provider,
                    Err(unavailable) => {
                        let reason = unavailable.to_protocol_message();
                        warn!(
                            job_id = %req.job_id,
                            tool = %req.tool,
                            reason = %reason,
                            "IPC: job rejected by capability provider"
                        );
                        let _ =
                            tx.send(job_capability_rejection_envelope(&device_id, &req, reason));
                        continue;
                    }
                };
                if req.interactive && matches!(job_provider, JobProvider::ManagedRuntime { .. }) {
                    warn!(
                        job_id = %req.job_id,
                        tool = %req.tool,
                        "IPC: managed runtime job rejected because interactive PTY is unsupported"
                    );
                    let _ = tx.send(managed_runtime_interactive_rejection_envelope(
                        &device_id, &req,
                    ));
                    continue;
                }

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

                // Session mode check.
                match session_mgr.check(&req, &caller_id).await {
                    SessionDecision::Deny(reason) => {
                        warn!(job_id = %req.job_id, reason = %reason, "IPC: job rejected by session mode");
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
                    SessionDecision::Allow => {
                        let job_id = req.job_id.clone();
                        let tx_clone = tx.clone();
                        let did = device_id.clone();
                        let reg = Arc::clone(&registry);
                        let st = store.clone();
                        let provider = job_provider.clone();

                        let (cancel_tx, cancel_rx) = mpsc::channel(1);
                        reg.register(job_id.clone(), cancel_tx).await;

                        let active = reg.active_count().await;
                        info!(job_id = %job_id, active_jobs = active, "IPC: job accepted");

                        tokio::spawn(async move {
                            let _permit = reg.acquire_permit().await;
                            let (exit_code, error) =
                                run_job_with_provider(did, req, provider, tx_clone, cancel_rx, st)
                                    .await;
                            reg.remove(&job_id).await;
                            reg.mark_completed(job_id, exit_code, error).await;
                        });
                    }
                    SessionDecision::NeedsApproval {
                        reason,
                        previous_refusals,
                    } => {
                        info!(job_id = %req.job_id, reason = %reason, "IPC: job needs approval (strict mode)");

                        let (approval_req, approval_rx) = approval_mgr
                            .submit(req.clone(), &caller_id, reason, previous_refusals)
                            .await;

                        // Send ApprovalRequest to this IPC client.
                        let approval_env = Envelope {
                            device_id: device_id.clone(),
                            msg_id: new_msg_id(),
                            ts_ms: now_ms(),
                            payload: Some(envelope::Payload::ApprovalRequest(approval_req.clone())),
                            ..Default::default()
                        };
                        let _ = tx.send(approval_env.clone());

                        // Also broadcast to other IPC clients.
                        let _ = approval_broadcast_tx.send(approval_env);

                        // Spawn a task to wait for approval.
                        let tx_clone = tx.clone();
                        let did = device_id.clone();
                        let reg = Arc::clone(&registry);
                        let st = store.clone();
                        let amgr = Arc::clone(&approval_mgr);
                        let smgr = Arc::clone(&session_mgr);
                        let timeout = amgr.default_timeout();
                        let job_id = req.job_id.clone();
                        let cuid = caller_id.clone();
                        let provider = job_provider.clone();

                        tokio::spawn(async move {
                            let result = tokio::time::timeout(timeout, approval_rx).await;
                            match result {
                                Ok(Ok(resp)) if resp.approved => {
                                    info!(job_id = %job_id, "IPC: approval granted");
                                    let (cancel_tx, cancel_rx) = mpsc::channel(1);
                                    reg.register(job_id.clone(), cancel_tx).await;
                                    let _permit = reg.acquire_permit().await;
                                    let (exit_code, error) = run_job_with_provider(
                                        did, req, provider, tx_clone, cancel_rx, st,
                                    )
                                    .await;
                                    reg.remove(&job_id).await;
                                    reg.mark_completed(job_id, exit_code, error).await;
                                }
                                Ok(Ok(resp)) => {
                                    info!(job_id = %job_id, "IPC: approval denied");
                                    if !resp.reason.is_empty() {
                                        smgr.record_refusal(&cuid, &req.tool, &resp.reason).await;
                                    }
                                    amgr.expire(&job_id).await;
                                    let reject_env = Envelope {
                                        device_id: did,
                                        msg_id: new_msg_id(),
                                        ts_ms: now_ms(),
                                        payload: Some(envelope::Payload::JobRejected(
                                            JobRejected {
                                                job_id,
                                                reason: if resp.reason.is_empty() {
                                                    "approval denied".to_string()
                                                } else {
                                                    format!("approval denied: {}", resp.reason)
                                                },
                                            },
                                        )),
                                        ..Default::default()
                                    };
                                    let _ = tx_clone.send(reject_env);
                                }
                                _ => {
                                    info!(job_id = %job_id, "IPC: approval timed out");
                                    amgr.expire(&job_id).await;
                                    let reject_env = Envelope {
                                        device_id: did,
                                        msg_id: new_msg_id(),
                                        ts_ms: now_ms(),
                                        payload: Some(envelope::Payload::JobRejected(
                                            JobRejected {
                                                job_id,
                                                reason: "approval timed out".to_string(),
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
                if !resp.approved && !resp.reason.is_empty() {
                    if let Some((req, _)) = approval_mgr.resolve(&resp).await {
                        session_mgr
                            .record_refusal(&caller_id, &req.tool, &resp.reason)
                            .await;
                    }
                } else {
                    approval_mgr.resolve(&resp).await;
                }
            }
            Some(envelope::Payload::SetSessionMode(msg)) => {
                let mode = SessionMode::try_from(msg.mode).unwrap_or(SessionMode::Inactive);
                info!(caller_uid = %msg.caller_uid, ?mode, "IPC: received set session mode");
                let state = session_mgr
                    .set_mode(&msg.caller_uid, mode, msg.trust_timeout_mins)
                    .await;
                let state_env = Envelope {
                    device_id: device_id.clone(),
                    msg_id: new_msg_id(),
                    ts_ms: now_ms(),
                    payload: Some(envelope::Payload::SessionState(state)),
                    ..Default::default()
                };
                let _ = tx.send(state_env);
            }
            Some(envelope::Payload::SessionQuery(query)) => {
                info!(caller_uid = %query.caller_uid, "IPC: received session query");
                let states = session_mgr.query_sessions(&query.caller_uid).await;
                for state in states {
                    let state_env = Envelope {
                        device_id: device_id.clone(),
                        msg_id: new_msg_id(),
                        ts_ms: now_ms(),
                        payload: Some(envelope::Payload::SessionState(state)),
                        ..Default::default()
                    };
                    let _ = tx.send(state_env);
                }
            }
            Some(envelope::Payload::BrowserRequest(req)) => {
                info!(
                    request_id = %req.request_id,
                    action = %req.action,
                    "IPC: received browser request"
                );

                let provider_registry = match crate::plugin_runtime::build_provider_registry(
                    &browser_mgr,
                    &file_mgr,
                )
                .await
                {
                    Ok(registry) => registry,
                    Err(err) => {
                        let resp_env = Envelope {
                            device_id: device_id.clone(),
                            msg_id: new_msg_id(),
                            ts_ms: now_ms(),
                            payload: Some(envelope::Payload::BrowserResponse(BrowserResponse {
                                request_id: req.request_id.clone(),
                                session_id: req.session_id.clone(),
                                success: false,
                                error: format!(
                                    "browser capability unavailable: failed to inspect host resources: {err}"
                                ),
                                ..Default::default()
                            })),
                            ..Default::default()
                        };
                        let _ = tx.send(resp_env);
                        continue;
                    }
                };

                if let Err(unavailable) = provider_registry.ensure(CapabilityKind::Browser) {
                    let resp_env = Envelope {
                        device_id: device_id.clone(),
                        msg_id: new_msg_id(),
                        ts_ms: now_ms(),
                        payload: Some(envelope::Payload::BrowserResponse(
                            browser_unavailable_response(&req, &unavailable),
                        )),
                        ..Default::default()
                    };
                    let _ = tx.send(resp_env);
                } else if let Err(reason) = browser_mgr.check_domain(&req.action, &req.params_json)
                {
                    let resp_env = Envelope {
                        device_id: device_id.clone(),
                        msg_id: new_msg_id(),
                        ts_ms: now_ms(),
                        payload: Some(envelope::Payload::BrowserResponse(BrowserResponse {
                            request_id: req.request_id.clone(),
                            session_id: req.session_id.clone(),
                            success: false,
                            error: reason,
                            ..Default::default()
                        })),
                        ..Default::default()
                    };
                    let _ = tx.send(resp_env);
                } else {
                    let bmgr = Arc::clone(&browser_mgr);
                    let did = device_id.clone();
                    let tx_clone = tx.clone();
                    tokio::spawn(async move {
                        let result = bmgr
                            .execute(
                                &req.session_id,
                                &req.action,
                                &req.params_json,
                                req.timeout_ms,
                            )
                            .await;
                        let resp = match result {
                            Ok(r) => BrowserResponse {
                                request_id: req.request_id.clone(),
                                session_id: req.session_id.clone(),
                                success: r.success,
                                result_json: r.result_json,
                                error: r.error,
                                binary_data: r.binary_data,
                                binary_mime: r.binary_mime,
                            },
                            Err(e) => BrowserResponse {
                                request_id: req.request_id.clone(),
                                session_id: req.session_id.clone(),
                                success: false,
                                error: format!("browser command failed: {}", e),
                                ..Default::default()
                            },
                        };
                        if req.action == "close" {
                            bmgr.release_session(&req.session_id).await;
                        }
                        let resp_env = Envelope {
                            device_id: did,
                            msg_id: new_msg_id(),
                            ts_ms: now_ms(),
                            payload: Some(envelope::Payload::BrowserResponse(resp)),
                            ..Default::default()
                        };
                        let _ = tx_clone.send(resp_env);
                    });
                }
            }
            _ => {}
        }
    }

    drop(tx);
    let _ = send_handle.await;
    Ok(())
}

fn job_capability_rejection_envelope(
    device_id: &str,
    req: &ahand_protocol::JobRequest,
    reason: String,
) -> Envelope {
    Envelope {
        device_id: device_id.to_string(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::JobRejected(JobRejected {
            job_id: req.job_id.clone(),
            reason,
        })),
        ..Default::default()
    }
}

fn managed_runtime_interactive_rejection_envelope(
    device_id: &str,
    req: &ahand_protocol::JobRequest,
) -> Envelope {
    Envelope {
        device_id: device_id.to_string(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::JobRejected(JobRejected {
            job_id: req.job_id.clone(),
            reason: format!(
                "managed runtime tool {} does not support interactive PTY jobs",
                req.tool
            ),
        })),
        ..Default::default()
    }
}

async fn run_job_with_provider(
    device_id: String,
    req: ahand_protocol::JobRequest,
    provider: JobProvider,
    tx: mpsc::UnboundedSender<Envelope>,
    cancel_rx: mpsc::Receiver<()>,
    store: Option<Arc<RunStore>>,
) -> (i32, String) {
    match provider {
        JobProvider::DefaultExec => executor::run_job(device_id, req, tx, cancel_rx, store).await,
        JobProvider::ManagedRuntime { target, .. } => {
            executor::run_job_with_target(device_id, req, target, tx, cancel_rx, store).await
        }
    }
}

fn browser_unavailable_response(
    req: &ahand_protocol::BrowserRequest,
    unavailable: &CapabilityUnavailable,
) -> BrowserResponse {
    BrowserResponse {
        request_id: req.request_id.clone(),
        session_id: req.session_id.clone(),
        success: false,
        error: unavailable.to_protocol_message(),
        ..Default::default()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_browser_unavailable_response_preserves_ids() {
        let req = ahand_protocol::BrowserRequest {
            request_id: "ipc-browser-req-1".to_string(),
            session_id: "ipc-browser-session-1".to_string(),
            action: "navigate".to_string(),
            ..Default::default()
        };
        let unavailable = crate::plugin_runtime::CapabilityUnavailable {
            capability: crate::plugin_runtime::CapabilityKind::Browser,
            plugin_id: "browser-playwright-cli".to_string(),
            status: crate::plugin_runtime::PluginStatus::Blocked,
            reason: "dependency node is missing".to_string(),
            remediation: crate::plugin_runtime::CapabilityRemediation::InstallPlugin {
                plugin_id: "browser-playwright-cli".to_string(),
            },
        };

        let resp = browser_unavailable_response(&req, &unavailable);

        assert_eq!(resp.request_id, "ipc-browser-req-1");
        assert_eq!(resp.session_id, "ipc-browser-session-1");
        assert!(!resp.success);
        assert!(
            resp.error.contains(
                "install plugin browser-playwright-cli through the host plugin installer"
            )
        );
    }

    #[test]
    fn ipc_job_capability_rejection_preserves_job_id() {
        let req = ahand_protocol::JobRequest {
            job_id: "ipc-job-1".to_string(),
            tool: "shell".to_string(),
            ..Default::default()
        };

        let env = job_capability_rejection_envelope(
            "device-1",
            &req,
            "exec capability unavailable: host shell unavailable".to_string(),
        );

        assert_eq!(env.device_id, "device-1");
        match env.payload {
            Some(envelope::Payload::JobRejected(rejected)) => {
                assert_eq!(rejected.job_id, "ipc-job-1");
                assert!(
                    rejected
                        .reason
                        .contains("exec capability unavailable: host shell unavailable")
                );
            }
            other => panic!("expected JobRejected envelope, got {other:?}"),
        }
    }

    #[test]
    fn ipc_managed_runtime_interactive_rejection_preserves_job_id() {
        let req = ahand_protocol::JobRequest {
            job_id: "ipc-python-interactive-1".to_string(),
            tool: "plugin:python".to_string(),
            interactive: true,
            ..Default::default()
        };

        let env = managed_runtime_interactive_rejection_envelope("device-1", &req);

        assert_eq!(env.device_id, "device-1");
        match env.payload {
            Some(envelope::Payload::JobRejected(rejected)) => {
                assert_eq!(rejected.job_id, "ipc-python-interactive-1");
                assert!(rejected.reason.contains("plugin:python"));
                assert!(rejected.reason.contains("interactive"));
            }
            other => panic!("expected JobRejected envelope, got {other:?}"),
        }
    }
}
