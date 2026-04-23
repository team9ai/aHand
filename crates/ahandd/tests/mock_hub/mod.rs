//! Minimal in-process WebSocket hub used by `lib_spawn` integration tests.
//!
//! Two server flavours are provided:
//!   * [`start_accepting`] — completes the `HelloChallenge` → `Hello` →
//!     `HelloAccepted` handshake, then holds the connection open quietly.
//!   * [`start_rejecting_401`] — sends `HelloChallenge`, reads the client's
//!     `Hello`, then closes with a `Policy("auth-rejected")` close frame
//!     (the same signal the real hub uses for auth failure).
//!
//! Keep this module small and self-contained — it exists so the daemon's
//! status state machine has something to race against, not to model the
//! full hub protocol.

#![allow(dead_code)]

use ahand_protocol::{Envelope, Heartbeat, HelloAccepted, HelloChallenge, envelope};
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::{
    Message as WsMessage,
    protocol::{CloseFrame, frame::coding::CloseCode},
};

/// Handle returned by `start_*` helpers. Drop stops the listener task.
pub struct Mock {
    pub port: u16,
    heartbeats: Arc<Mutex<Vec<Heartbeat>>>,
    _shutdown: oneshot::Sender<()>,
    _task: JoinHandle<()>,
}

impl Mock {
    pub fn ws_url(&self) -> String {
        format!("ws://127.0.0.1:{}/ws", self.port)
    }

    pub fn valid_jwt(&self) -> String {
        "test-bootstrap-token".to_string()
    }

    /// Snapshot of every `Heartbeat` envelope observed from any connected
    /// daemon since the mock started. Cheap to clone; intended for
    /// integration-test assertions, not for high-volume production use.
    pub fn captured_heartbeats(&self) -> Vec<Heartbeat> {
        self.heartbeats.lock().unwrap().clone()
    }
}

/// Start a mock hub that accepts every Hello.
pub async fn start_accepting() -> Mock {
    start(Behavior::Accept).await
}

/// Start a mock hub that rejects every Hello with an `auth-rejected` close frame.
pub async fn start_rejecting_401() -> Mock {
    start(Behavior::RejectAuth).await
}

#[derive(Clone, Copy)]
enum Behavior {
    Accept,
    RejectAuth,
}

async fn start(behavior: Behavior) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("local_addr").port();
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
    let heartbeats: Arc<Mutex<Vec<Heartbeat>>> = Arc::new(Mutex::new(Vec::new()));

    let heartbeats_for_task = heartbeats.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accept = listener.accept() => {
                    let (stream, _) = match accept {
                        Ok(pair) => pair,
                        Err(_) => break,
                    };
                    tokio::spawn(handle_conn(stream, behavior, heartbeats_for_task.clone()));
                }
            }
        }
    });

    Mock {
        port,
        heartbeats,
        _shutdown: shutdown_tx,
        _task: task,
    }
}

async fn handle_conn(
    stream: tokio::net::TcpStream,
    behavior: Behavior,
    heartbeats: Arc<Mutex<Vec<Heartbeat>>>,
) {
    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    let (mut sink, mut src) = ws.split();

    // 1. Push a HelloChallenge so the client can respond with a signed Hello.
    let challenge = Envelope {
        device_id: "mock-hub".into(),
        msg_id: "challenge-0".into(),
        ts_ms: 0,
        payload: Some(envelope::Payload::HelloChallenge(HelloChallenge {
            nonce: b"mock-nonce-1234".to_vec(),
            issued_at_ms: 0,
        })),
        ..Default::default()
    };
    if sink
        .send(WsMessage::Binary(challenge.encode_to_vec()))
        .await
        .is_err()
    {
        return;
    }

    // 2. Read the client's Hello.
    let Some(Ok(msg)) = src.next().await else {
        return;
    };
    let WsMessage::Binary(_hello_bytes) = msg else {
        return;
    };

    // 3. Respond according to the configured behavior.
    match behavior {
        Behavior::Accept => {
            let accepted = Envelope {
                device_id: "mock-hub".into(),
                msg_id: "accepted-0".into(),
                ts_ms: 0,
                payload: Some(envelope::Payload::HelloAccepted(HelloAccepted {
                    auth_method: "bootstrap".into(),
                    update_suggestion: None,
                })),
                ..Default::default()
            };
            let _ = sink.send(WsMessage::Binary(accepted.encode_to_vec())).await;
            // Keep the connection open until the client closes it, and
            // record every `Heartbeat` envelope observed on the way.
            while let Some(m) = src.next().await {
                let frame = match m {
                    Ok(WsMessage::Binary(bytes)) => bytes,
                    Ok(_) => continue,
                    Err(_) => break,
                };
                let Ok(envelope) = Envelope::decode(frame.as_ref()) else {
                    continue;
                };
                if let Some(envelope::Payload::Heartbeat(hb)) = envelope.payload {
                    heartbeats.lock().unwrap().push(hb);
                }
            }
        }
        Behavior::RejectAuth => {
            let _ = sink
                .send(WsMessage::Close(Some(CloseFrame {
                    code: CloseCode::Policy,
                    reason: Cow::Borrowed("auth-rejected"),
                })))
                .await;
            let _ = sink.close().await;
        }
    }
}
