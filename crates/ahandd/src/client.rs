use std::sync::Arc;

use ahand_protocol::{envelope, Envelope, Hello, JobFinished, JobRejected};
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::executor;
use crate::outbox::{prepare_outbound, Outbox};
use crate::policy::PolicyChecker;
use crate::registry::{IsKnown, JobRegistry};
use crate::store::{Direction, RunStore};

pub async fn run(
    config: Config,
    device_id: String,
    registry: Arc<JobRegistry>,
    store: Option<Arc<RunStore>>,
    policy: Arc<PolicyChecker>,
) -> anyhow::Result<()> {

    // Outbox survives across reconnects.
    let outbox = Arc::new(tokio::sync::Mutex::new(Outbox::new(10_000)));

    let mut backoff = 1u64;

    loop {
        info!(url = %config.server_url, "connecting to cloud");

        match connect(
            &config.server_url,
            &device_id,
            &policy,
            &registry,
            &store,
            &outbox,
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

async fn connect(
    url: &str,
    device_id: &str,
    policy: &Arc<PolicyChecker>,
    registry: &Arc<JobRegistry>,
    store: &Option<Arc<RunStore>>,
    outbox: &Arc<tokio::sync::Mutex<Outbox>>,
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
    sink.send(tungstenite::Message::Binary(data.into())).await?;

    // Replay unacked messages from previous connection.
    let unacked = outbox.lock().await.drain_unacked();
    if !unacked.is_empty() {
        info!(count = unacked.len(), "replaying unacked messages");
        for data in unacked {
            sink.send(tungstenite::Message::Binary(data.into()))
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
                .send(tungstenite::Message::Binary(data.into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

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
                // Idempotency check: skip if already running or completed.
                match registry.is_known(&req.job_id).await {
                    IsKnown::Running => {
                        warn!(job_id = %req.job_id, "duplicate job_id, already running — ignoring");
                        continue;
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
                        continue;
                    }
                    IsKnown::Unknown => {}
                }

                // Check policy.
                if let Err(reason) = policy.check(&req) {
                    warn!(job_id = %req.job_id, reason = %reason, "job rejected by policy");
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
                } else {
                    let job_id = req.job_id.clone();
                    let tx_clone = tx.clone();
                    let did = device_id.to_string();
                    let reg = Arc::clone(registry);
                    let st = store.clone();

                    // Create cancel channel.
                    let (cancel_tx, cancel_rx) = mpsc::channel(1);
                    reg.register(job_id.clone(), cancel_tx).await;

                    let active = reg.active_count().await;
                    info!(job_id = %job_id, active_jobs = active, "job accepted, acquiring permit");

                    // Spawn the job — acquire permit first (may wait if at capacity).
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
                info!(job_id = %cancel.job_id, "received cancel request");
                registry.cancel(&cancel.job_id).await;
            }
            _ => {}
        }
    }

    // Drop tx so the send task exits.
    drop(tx);
    let _ = send_handle.await;

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
    format!("d-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}
