use std::sync::Arc;

use ahand_protocol::{envelope, Envelope, Hello, JobFinished, JobRejected};
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite;
use tracing::{error, info, warn};

use crate::approval::ApprovalManager;
use crate::config::Config;
use crate::executor;
use crate::outbox::{prepare_outbound, Outbox};
use crate::registry::{IsKnown, JobRegistry};
use crate::session::{SessionDecision, SessionManager};
use crate::store::{Direction, RunStore};

#[allow(clippy::too_many_arguments)]
pub async fn run(
    config: Config,
    device_id: String,
    registry: Arc<JobRegistry>,
    store: Option<Arc<RunStore>>,
    session_mgr: Arc<SessionManager>,
    approval_mgr: Arc<ApprovalManager>,
    approval_broadcast_tx: broadcast::Sender<Envelope>,
) -> anyhow::Result<()> {

    // Outbox survives across reconnects.
    let outbox = Arc::new(tokio::sync::Mutex::new(Outbox::new(10_000)));

    let mut backoff = 1u64;

    loop {
        info!(url = %config.server_url, "connecting to cloud");

        match connect(
            &config.server_url,
            &device_id,
            &session_mgr,
            &registry,
            &store,
            &outbox,
            &approval_mgr,
            &approval_broadcast_tx,
        )
        .await
        {
            Ok(()) => {
                info!("disconnected from cloud");
                backoff = 1;
            }
            Err(e) => {
                warn!(error = %e, "connection failed");
            }
        }

        let delay = std::time::Duration::from_secs(backoff);
        info!(delay_secs = backoff, "reconnecting after delay");
        tokio::time::sleep(delay).await;
        backoff = (backoff * 2).min(30);
    }
}

#[allow(clippy::too_many_arguments)]
async fn connect(
    url: &str,
    device_id: &str,
    session_mgr: &Arc<SessionManager>,
    registry: &Arc<JobRegistry>,
    store: &Option<Arc<RunStore>>,
    outbox: &Arc<tokio::sync::Mutex<Outbox>>,
    approval_mgr: &Arc<ApprovalManager>,
    approval_broadcast_tx: &broadcast::Sender<Envelope>,
) -> anyhow::Result<()> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(url).await?;
    let (mut sink, mut stream) = ws_stream.split();

    let last_ack = outbox.lock().await.local_ack();
    info!(last_ack, "connected, sending Hello");

    // Send Hello envelope — Hello is NOT stamped (seq=0), it's a connection signal.
    let hello = Envelope {
        device_id: device_id.to_string(),
        msg_id: "hello-0".to_string(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::Hello(Hello {
            version: env!("CARGO_PKG_VERSION").to_string(),
            hostname: gethostname::gethostname()
                .to_string_lossy()
                .to_string(),
            os: std::env::consts::OS.to_string(),
            capabilities: vec!["exec".to_string()],
            last_ack,
        })),
        ..Default::default()
    };
    let data = hello.encode_to_vec();
    if let Some(s) = store {
        s.log_envelope(&hello, Direction::Outbound).await;
    }
    sink.send(tungstenite::Message::Binary(data)).await?;

    // Replay unacked messages from previous connection.
    let unacked = outbox.lock().await.drain_unacked();
    if !unacked.is_empty() {
        info!(count = unacked.len(), "replaying unacked messages");
        for data in unacked {
            sink.send(tungstenite::Message::Binary(data))
                .await?;
        }
    }

    // Channel: executor sends Envelope objects, send task stamps + encodes + sends.
    let (tx, mut rx) = mpsc::unbounded_channel::<Envelope>();

    let store_send = store.clone();
    let outbox_send = Arc::clone(outbox);

    // Task: receive Envelope from executors, stamp with outbox, encode, send over WS.
    let send_handle = tokio::spawn(async move {
        while let Some(mut envelope) = rx.recv().await {
            let data = {
                let mut ob = outbox_send.lock().await;
                prepare_outbound(&mut ob, &mut envelope)
            };

            // Log outbound envelopes to trace.
            if let Some(s) = &store_send {
                s.log_envelope(&envelope, Direction::Outbound).await;
            }
            if sink
                .send(tungstenite::Message::Binary(data))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let caller_uid = "cloud";

    // Register the cloud caller so session queries return it.
    session_mgr.register_caller(caller_uid).await;

    // Process incoming messages.
    while let Some(msg) = stream.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                error!(error = %e, "websocket read error");
                break;
            }
        };

        let data = match msg {
            tungstenite::Message::Binary(b) => b,
            tungstenite::Message::Close(_) => break,
            _ => continue,
        };

        let envelope = match Envelope::decode(data.as_ref()) {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "failed to decode envelope");
                continue;
            }
        };

        // Log inbound envelope to trace.
        if let Some(s) = store {
            s.log_envelope(&envelope, Direction::Inbound).await;
        }

        // Update outbox with peer's seq and ack.
        {
            let mut ob = outbox.lock().await;
            if envelope.seq > 0 {
                ob.on_recv(envelope.seq);
            }
            if envelope.ack > 0 {
                ob.on_peer_ack(envelope.ack);
            }
        }

        match envelope.payload {
            Some(envelope::Payload::JobRequest(req)) => {
                handle_job_request(
                    req,
                    device_id,
                    caller_uid,
                    &tx,
                    session_mgr,
                    registry,
                    store,
                    approval_mgr,
                    approval_broadcast_tx,
                )
                .await;
            }
            Some(envelope::Payload::CancelJob(cancel)) => {
                info!(job_id = %cancel.job_id, "received cancel request");
                registry.cancel(&cancel.job_id).await;
            }
            Some(envelope::Payload::ApprovalResponse(resp)) => {
                info!(job_id = %resp.job_id, approved = resp.approved, "received approval response from cloud");
                // Record refusal if reason is provided.
                if !resp.approved && !resp.reason.is_empty() {
                    if let Some((req, _)) = approval_mgr.resolve(&resp).await {
                        session_mgr
                            .record_refusal(caller_uid, &req.tool, &resp.reason)
                            .await;
                    }
                } else {
                    approval_mgr.resolve(&resp).await;
                }
            }
            Some(envelope::Payload::SetSessionMode(msg)) => {
                handle_set_session_mode(device_id, session_mgr, &msg, &tx).await;
            }
            Some(envelope::Payload::SessionQuery(query)) => {
                handle_session_query(device_id, session_mgr, &query, &tx).await;
            }
            _ => {}
        }
    }

    // Drop tx so the send task exits.
    drop(tx);
    let _ = send_handle.await;

    Ok(())
}

/// Handle an incoming JobRequest with idempotency + session mode check.
#[allow(clippy::too_many_arguments)]
async fn handle_job_request(
    req: ahand_protocol::JobRequest,
    device_id: &str,
    caller_uid: &str,
    tx: &mpsc::UnboundedSender<Envelope>,
    session_mgr: &Arc<SessionManager>,
    registry: &Arc<JobRegistry>,
    store: &Option<Arc<RunStore>>,
    approval_mgr: &Arc<ApprovalManager>,
    approval_broadcast_tx: &broadcast::Sender<Envelope>,
) {
    // Idempotency check.
    match registry.is_known(&req.job_id).await {
        IsKnown::Running => {
            warn!(job_id = %req.job_id, "duplicate job_id, already running — ignoring");
            return;
        }
        IsKnown::Completed(c) => {
            info!(job_id = %req.job_id, "duplicate job_id, returning cached result");
            let finished_env = Envelope {
                device_id: device_id.to_string(),
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
            return;
        }
        IsKnown::Unknown => {}
    }

    // Session mode check.
    match session_mgr.check(&req, caller_uid).await {
        SessionDecision::Deny(reason) => {
            warn!(job_id = %req.job_id, reason = %reason, "job rejected by session mode");
            let reject_env = Envelope {
                device_id: device_id.to_string(),
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
            spawn_job(device_id, req, tx, registry, store).await;
        }
        SessionDecision::NeedsApproval { reason, previous_refusals } => {
            info!(job_id = %req.job_id, reason = %reason, "job needs approval (strict mode)");

            let (approval_req, approval_rx) = approval_mgr
                .submit(req.clone(), caller_uid, reason, previous_refusals)
                .await;

            // Send ApprovalRequest to cloud via WS.
            let approval_env = Envelope {
                device_id: device_id.to_string(),
                msg_id: new_msg_id(),
                ts_ms: now_ms(),
                payload: Some(envelope::Payload::ApprovalRequest(approval_req.clone())),
                ..Default::default()
            };
            let _ = tx.send(approval_env.clone());

            // Broadcast to all IPC clients.
            let _ = approval_broadcast_tx.send(approval_env);

            // Spawn a task to wait for approval.
            let tx_clone = tx.clone();
            let did = device_id.to_string();
            let reg = Arc::clone(registry);
            let st = store.clone();
            let amgr = Arc::clone(approval_mgr);
            let smgr = Arc::clone(session_mgr);
            let timeout = amgr.default_timeout();
            let job_id = req.job_id.clone();
            let cuid = caller_uid.to_string();

            tokio::spawn(async move {
                let result = tokio::time::timeout(timeout, approval_rx).await;
                match result {
                    Ok(Ok(resp)) if resp.approved => {
                        info!(job_id = %job_id, "approval granted");
                        spawn_job(&did, req, &tx_clone, &reg, &st).await;
                    }
                    Ok(Ok(resp)) => {
                        // Denied — record refusal if reason provided.
                        info!(job_id = %job_id, "approval denied");
                        if !resp.reason.is_empty() {
                            smgr.record_refusal(&cuid, &req.tool, &resp.reason).await;
                        }
                        amgr.expire(&job_id).await;
                        let reject_env = Envelope {
                            device_id: did,
                            msg_id: new_msg_id(),
                            ts_ms: now_ms(),
                            payload: Some(envelope::Payload::JobRejected(JobRejected {
                                job_id,
                                reason: if resp.reason.is_empty() {
                                    "approval denied".to_string()
                                } else {
                                    format!("approval denied: {}", resp.reason)
                                },
                            })),
                            ..Default::default()
                        };
                        let _ = tx_clone.send(reject_env);
                    }
                    _ => {
                        info!(job_id = %job_id, "approval timed out");
                        amgr.expire(&job_id).await;
                        let reject_env = Envelope {
                            device_id: did,
                            msg_id: new_msg_id(),
                            ts_ms: now_ms(),
                            payload: Some(envelope::Payload::JobRejected(JobRejected {
                                job_id,
                                reason: "approval timed out".to_string(),
                            })),
                            ..Default::default()
                        };
                        let _ = tx_clone.send(reject_env);
                    }
                }
            });
        }
    }
}

/// Spawn a job execution task.
async fn spawn_job(
    device_id: &str,
    req: ahand_protocol::JobRequest,
    tx: &mpsc::UnboundedSender<Envelope>,
    registry: &Arc<JobRegistry>,
    store: &Option<Arc<RunStore>>,
) {
    let job_id = req.job_id.clone();
    let tx_clone = tx.clone();
    let did = device_id.to_string();
    let reg = Arc::clone(registry);
    let st = store.clone();

    let (cancel_tx, cancel_rx) = mpsc::channel(1);
    reg.register(job_id.clone(), cancel_tx).await;

    let active = reg.active_count().await;
    info!(job_id = %job_id, active_jobs = active, "job accepted, acquiring permit");

    tokio::spawn(async move {
        let _permit = reg.acquire_permit().await;
        let (exit_code, error) =
            executor::run_job(did, req, tx_clone, cancel_rx, st).await;
        reg.remove(&job_id).await;
        reg.mark_completed(job_id, exit_code, error).await;
    });
}

async fn handle_set_session_mode(
    device_id: &str,
    session_mgr: &Arc<SessionManager>,
    msg: &ahand_protocol::SetSessionMode,
    tx: &mpsc::UnboundedSender<Envelope>,
) {
    let mode = ahand_protocol::SessionMode::try_from(msg.mode).unwrap_or(ahand_protocol::SessionMode::Inactive);
    info!(caller_uid = %msg.caller_uid, ?mode, "received set session mode");
    let state = session_mgr
        .set_mode(&msg.caller_uid, mode, msg.trust_timeout_mins)
        .await;
    let state_env = Envelope {
        device_id: device_id.to_string(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::SessionState(state)),
        ..Default::default()
    };
    let _ = tx.send(state_env);
}

async fn handle_session_query(
    device_id: &str,
    session_mgr: &Arc<SessionManager>,
    query: &ahand_protocol::SessionQuery,
    tx: &mpsc::UnboundedSender<Envelope>,
) {
    info!(caller_uid = %query.caller_uid, "received session query");
    let states = session_mgr.query_sessions(&query.caller_uid).await;
    for state in states {
        let state_env = Envelope {
            device_id: device_id.to_string(),
            msg_id: new_msg_id(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::SessionState(state)),
            ..Default::default()
        };
        let _ = tx.send(state_env);
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
