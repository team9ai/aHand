//! Minimal in-process WebSocket hub used by `lib_spawn` integration tests.
//!
//! Server flavours:
//!   * [`start_accepting`] — completes the `HelloChallenge` → `Hello` →
//!     `HelloAccepted` handshake, then holds the connection open quietly.
//!   * [`start_rejecting_401`] — sends `HelloChallenge`, reads the client's
//!     `Hello`, then closes with a `Policy("auth-rejected")` close frame
//!     (the same signal the real hub uses for auth failure).
//!   * [`start_silent_after_handshake`] — accepts handshake then stops
//!     reading, leaving the WS in a half-zombie state for the watchdog to
//!     catch.
//!   * [`start_with_file_request`] — accepts handshake, immediately injects
//!     a [`FileRequest`] envelope and captures the daemon's [`FileResponse`].
//!     Used to exercise the daemon's `handle_file_request` glue end-to-end
//!     through the WS layer.
//!   * [`start_accepting_drop_after_n_snapshots`] — like `start_accepting`
//!     but drops the first connection after receiving `n` `AppToolsUpdate`
//!     snapshots, letting the daemon reconnect.
//!
//! Keep this module small and self-contained — it exists so the daemon's
//! status state machine has something to race against, not to model the
//! full hub protocol.

#![allow(dead_code)]

use ahand_protocol::{
    AppToolsUpdate, Envelope, FileRequest, FileResponse, Heartbeat, HelloAccepted, HelloChallenge,
    envelope,
};
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
    file_responses: Arc<Mutex<Vec<FileResponse>>>,
    app_tools_updates: Arc<Mutex<Vec<AppToolsUpdate>>>,
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

    /// Snapshot of every `FileResponse` envelope observed from connected
    /// daemons. Populated by `start_with_file_request`-style mock servers
    /// that inject a `FileRequest` and capture the daemon's reply.
    pub fn captured_file_responses(&self) -> Vec<FileResponse> {
        self.file_responses.lock().unwrap().clone()
    }

    /// Snapshot of every `AppToolsUpdate` envelope received from connected
    /// daemons since the mock started (across all connections/reconnects).
    pub fn captured_app_tools_updates(&self) -> Vec<AppToolsUpdate> {
        self.app_tools_updates.lock().unwrap().clone()
    }

    /// Wait until at least `n` `AppToolsUpdate` envelopes have been
    /// received, then return all of them. Polls with a small sleep to avoid
    /// spinning. Returns `None` on timeout.
    pub async fn wait_for_app_tools_updates(
        &self,
        n: usize,
        timeout: std::time::Duration,
    ) -> Option<Vec<AppToolsUpdate>> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let updates = self.captured_app_tools_updates();
            if updates.len() >= n {
                return Some(updates);
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }
}

/// Start a mock hub that accepts every Hello.
pub async fn start_accepting() -> Mock {
    start(Behavior::Accept {
        drop_after_n_app_tools_updates: None,
    })
    .await
}

/// Start a mock hub that rejects every Hello with an `auth-rejected` close frame.
pub async fn start_rejecting_401() -> Mock {
    start(Behavior::RejectAuth).await
}

/// Start a mock hub that completes the handshake then **stops reading**
/// from the socket. tokio_tungstenite auto-Pongs only as a side effect of
/// reading frames via `stream.next()` — by never reading, our mock leaves
/// every client Ping in the TCP recv buffer with no Pong reply. From the
/// daemon's perspective the connection looks alive at the OS level
/// (writes still succeed into the send buffer) but no inbound activity
/// arrives. This is the zombie-TCP shape the watchdog is meant to catch.
///
/// We deliberately don't close the socket either — that would bypass the
/// watchdog entirely (the daemon's read loop would see Close and break
/// for that reason instead).
pub async fn start_silent_after_handshake() -> Mock {
    start(Behavior::SilentAfterHandshake).await
}

/// Start a mock hub that accepts the handshake, then immediately injects
/// the supplied `FileRequest` over the WebSocket. Captures every inbound
/// `FileResponse` the daemon sends back into `Mock::captured_file_responses()`.
/// Used to exercise the daemon's `handle_file_request` glue end-to-end —
/// from envelope decode in the read loop, through `FileManager::handle`,
/// back through the buffered envelope sender, and onto the wire as a
/// FileResponse envelope.
pub async fn start_with_file_request(req: FileRequest) -> Mock {
    start(Behavior::SendFileRequest(Arc::new(req))).await
}

/// Start a mock hub that accepts connections and drops the first connection
/// after receiving `n` `AppToolsUpdate` envelopes. Useful for reconnect
/// tests: the daemon will reconnect and re-send the snapshot after a new
/// Hello handshake.
pub async fn start_accepting_drop_after_n_snapshots(n: usize) -> Mock {
    start(Behavior::Accept {
        drop_after_n_app_tools_updates: Some(n),
    })
    .await
}

#[derive(Clone)]
enum Behavior {
    /// Accept every Hello and keep the connection open. If
    /// `drop_after_n_app_tools_updates` is `Some(n)`, close the connection
    /// after receiving that many `AppToolsUpdate` envelopes (used to trigger
    /// daemon reconnect in tests).
    Accept {
        drop_after_n_app_tools_updates: Option<usize>,
    },
    RejectAuth,
    SilentAfterHandshake,
    SendFileRequest(Arc<FileRequest>),
}

async fn start(behavior: Behavior) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("local_addr").port();
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
    let heartbeats: Arc<Mutex<Vec<Heartbeat>>> = Arc::new(Mutex::new(Vec::new()));
    let file_responses: Arc<Mutex<Vec<FileResponse>>> = Arc::new(Mutex::new(Vec::new()));
    let app_tools_updates: Arc<Mutex<Vec<AppToolsUpdate>>> = Arc::new(Mutex::new(Vec::new()));

    let heartbeats_for_task = heartbeats.clone();
    let file_responses_for_task = file_responses.clone();
    let app_tools_updates_for_task = app_tools_updates.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accept = listener.accept() => {
                    let (stream, _) = match accept {
                        Ok(pair) => pair,
                        Err(_) => break,
                    };
                    tokio::spawn(handle_conn(
                        stream,
                        behavior.clone(),
                        heartbeats_for_task.clone(),
                        file_responses_for_task.clone(),
                        app_tools_updates_for_task.clone(),
                    ));
                }
            }
        }
    });

    Mock {
        port,
        heartbeats,
        file_responses,
        app_tools_updates,
        _shutdown: shutdown_tx,
        _task: task,
    }
}

async fn handle_conn(
    stream: tokio::net::TcpStream,
    behavior: Behavior,
    heartbeats: Arc<Mutex<Vec<Heartbeat>>>,
    file_responses: Arc<Mutex<Vec<FileResponse>>>,
    app_tools_updates: Arc<Mutex<Vec<AppToolsUpdate>>>,
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
        Behavior::Accept {
            drop_after_n_app_tools_updates,
        } => {
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
            // record every `Heartbeat` and `AppToolsUpdate` envelope
            // observed on the way. If `drop_after_n_app_tools_updates` is
            // set, close the connection after that many AppToolsUpdate
            // messages (triggers a reconnect in the daemon).
            let mut app_tools_count = 0usize;
            while let Some(m) = src.next().await {
                let frame = match m {
                    Ok(WsMessage::Binary(bytes)) => bytes,
                    Ok(_) => continue,
                    Err(_) => break,
                };
                let Ok(envelope) = Envelope::decode(frame.as_ref()) else {
                    continue;
                };
                match envelope.payload {
                    Some(envelope::Payload::Heartbeat(hb)) => {
                        heartbeats.lock().unwrap().push(hb);
                    }
                    Some(envelope::Payload::AppToolsUpdate(update)) => {
                        app_tools_updates.lock().unwrap().push(update);
                        app_tools_count += 1;
                        if let Some(limit) = drop_after_n_app_tools_updates {
                            if app_tools_count >= limit {
                                // Send a WS Close frame so the daemon's read
                                // loop sees a clean close and reconnects.
                                let _ = sink
                                    .send(WsMessage::Close(Some(CloseFrame {
                                        code: CloseCode::Normal,
                                        reason: Cow::Borrowed("test-reconnect"),
                                    })))
                                    .await;
                                break;
                            }
                        }
                    }
                    _ => {}
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
        Behavior::SilentAfterHandshake => {
            let accepted = Envelope {
                device_id: "mock-hub".into(),
                msg_id: "accepted-zombie".into(),
                ts_ms: 0,
                payload: Some(envelope::Payload::HelloAccepted(HelloAccepted {
                    auth_method: "bootstrap".into(),
                    update_suggestion: None,
                })),
                ..Default::default()
            };
            let _ = sink.send(WsMessage::Binary(accepted.encode_to_vec())).await;

            // Hold sink + src open without ever reading — the WS is
            // alive at the TCP level but no Pong reply ever goes back
            // to the client. Sleep the task long enough for the
            // daemon's watchdog to fire (orders of magnitude longer
            // than any test's heartbeat_interval).
            let _keep_alive = (sink, src);
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
        Behavior::SendFileRequest(req) => {
            let accepted = Envelope {
                device_id: "mock-hub".into(),
                msg_id: "accepted-files".into(),
                ts_ms: 0,
                payload: Some(envelope::Payload::HelloAccepted(HelloAccepted {
                    auth_method: "bootstrap".into(),
                    update_suggestion: None,
                })),
                ..Default::default()
            };
            let _ = sink.send(WsMessage::Binary(accepted.encode_to_vec())).await;

            let file_req = Envelope {
                device_id: "mock-hub".into(),
                msg_id: "file-req-1".into(),
                ts_ms: 0,
                payload: Some(envelope::Payload::FileRequest((*req).clone())),
                ..Default::default()
            };
            let _ = sink.send(WsMessage::Binary(file_req.encode_to_vec())).await;

            // Capture every inbound envelope's FileResponse, plus
            // record heartbeats and AppToolsUpdate so existing assertions still work.
            while let Some(m) = src.next().await {
                let frame = match m {
                    Ok(WsMessage::Binary(bytes)) => bytes,
                    Ok(_) => continue,
                    Err(_) => break,
                };
                let Ok(envelope) = Envelope::decode(frame.as_ref()) else {
                    continue;
                };
                match envelope.payload {
                    Some(envelope::Payload::FileResponse(resp)) => {
                        file_responses.lock().unwrap().push(resp);
                    }
                    Some(envelope::Payload::Heartbeat(hb)) => {
                        heartbeats.lock().unwrap().push(hb);
                    }
                    Some(envelope::Payload::AppToolsUpdate(update)) => {
                        app_tools_updates.lock().unwrap().push(update);
                    }
                    _ => {}
                }
            }
        }
    }
}
