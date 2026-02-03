use ahand_protocol::{envelope, Envelope, Hello, JobRejected};
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::executor;
use crate::policy::PolicyChecker;

pub async fn run(config: Config) -> anyhow::Result<()> {
    let device_id = config.device_id();
    let policy = PolicyChecker::new(&config.policy);
    let mut backoff = 1u64;

    loop {
        info!(url = %config.server_url, "connecting to cloud");

        match connect(&config.server_url, &device_id, &policy).await {
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
    sink.send(tungstenite::Message::Binary(data.into())).await?;

    // Channel for sending responses back through the WebSocket.
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // Task: forward outgoing messages to the WebSocket sink.
    let send_handle = tokio::spawn(async move {
        while let Some(data) = rx.recv().await {
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

        if let Some(envelope::Payload::JobRequest(req)) = envelope.payload {
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
                let tx_clone = tx.clone();
                let did = device_id.to_string();
                tokio::spawn(async move {
                    executor::run_job(did, req, tx_clone).await;
                });
            }
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
