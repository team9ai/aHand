use std::sync::Arc;

use ahand_protocol::{envelope, Envelope, Hello, JobRejected};
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::executor;
use crate::policy::PolicyChecker;
use crate::registry::JobRegistry;
use crate::store::{Direction, RunStore};

pub async fn run(config: Config) -> anyhow::Result<()> {
    let device_id = config.device_id();
    let policy = PolicyChecker::new(&config.policy);

    let max_jobs = config.max_concurrent_jobs.unwrap_or(8);
    let registry = Arc::new(JobRegistry::new(max_jobs));

    let store = match config.data_dir() {
        Some(dir) => match RunStore::new(&dir) {
            Ok(s) => {
                info!(data_dir = %dir.display(), "run store initialised");
                Some(Arc::new(s))
            }
            Err(e) => {
                warn!(error = %e, "failed to initialise run store, persistence disabled");
                None
            }
        },
        None => None,
    };

    let mut backoff = 1u64;

    loop {
        info!(url = %config.server_url, "connecting to cloud");

        match connect(&config.server_url, &device_id, &policy, &registry, &store).await {
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
    policy: &PolicyChecker,
    registry: &Arc<JobRegistry>,
    store: &Option<Arc<RunStore>>,
) -> anyhow::Result<()> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(url).await?;
    let (mut sink, mut stream) = ws_stream.split();

    info!("connected, sending Hello");

    // Send Hello envelope.
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
        })),
        ..Default::default()
    };
    let data = hello.encode_to_vec();
    if let Some(s) = store {
        s.log_envelope(&hello, Direction::Outbound).await;
    }
    sink.send(tungstenite::Message::Binary(data.into())).await?;

    // Channel for sending responses back through the WebSocket.
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();

    let store_send = store.clone();

    // Task: forward outgoing messages to the WebSocket sink.
    let send_handle = tokio::spawn(async move {
        while let Some(data) = rx.recv().await {
            // Log outbound envelopes to trace.
            if let Some(s) = &store_send {
                if let Ok(env) = Envelope::decode(data.as_slice()) {
                    s.log_envelope(&env, Direction::Outbound).await;
                }
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

        match envelope.payload {
            Some(envelope::Payload::JobRequest(req)) => {
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
                    let _ = tx.send(reject_env.encode_to_vec());
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
                        executor::run_job(did, req, tx_clone, cancel_rx, st).await;
                        reg.remove(&job_id).await;
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
