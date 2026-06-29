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
    AppToolRequest, AppToolResponse, AppToolsUpdate, ApprovalRequest, ApprovalResponse, Envelope,
    FileRequest, FileResponse, Heartbeat, HelloAccepted, HelloChallenge, envelope,
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

/// mpsc sender used to inject envelopes into the active connection, paired
/// with a generation counter so stale connection cleanup can't clobber a
/// newer connection's sender.
type InjectSlot = Arc<Mutex<Option<(u64, tokio::sync::mpsc::UnboundedSender<Envelope>)>>>;

/// Handle returned by `start_*` helpers. Drop stops the listener task.
pub struct Mock {
    pub port: u16,
    heartbeats: Arc<Mutex<Vec<Heartbeat>>>,
    file_responses: Arc<Mutex<Vec<FileResponse>>>,
    app_tools_updates: Arc<Mutex<Vec<AppToolsUpdate>>>,
    app_tool_responses: Arc<Mutex<Vec<AppToolResponse>>>,
    /// Captured ApprovalRequest envelopes received from the daemon.
    approval_requests: Arc<Mutex<Vec<ApprovalRequest>>>,
    inject_tx: InjectSlot,
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

    /// Snapshot of every `AppToolResponse` envelope received from connected
    /// daemons since the mock started.
    pub fn captured_app_tool_responses(&self) -> Vec<AppToolResponse> {
        self.app_tool_responses.lock().unwrap().clone()
    }

    /// Snapshot of every `ApprovalRequest` envelope received from connected
    /// daemons since the mock started.
    pub fn captured_approval_requests(&self) -> Vec<ApprovalRequest> {
        self.approval_requests.lock().unwrap().clone()
    }

    /// Wait until at least `n` `ApprovalRequest` envelopes have been received,
    /// then return all of them. Returns `None` on timeout.
    pub async fn wait_for_approval_requests(
        &self,
        n: usize,
        timeout: std::time::Duration,
    ) -> Option<Vec<ApprovalRequest>> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let reqs = self.captured_approval_requests();
            if reqs.len() >= n {
                return Some(reqs);
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    /// Send an `ApprovalResponse` envelope to the connected daemon. Returns
    /// `Err` if no daemon is currently connected (no inject channel active).
    pub fn send_approval_response(
        &self,
        job_id: impl Into<String>,
        approved: bool,
        reason: impl Into<String>,
    ) -> Result<(), String> {
        let env = Envelope {
            device_id: "mock-hub".into(),
            msg_id: "approval-resp".into(),
            ts_ms: 0,
            payload: Some(envelope::Payload::ApprovalResponse(ApprovalResponse {
                job_id: job_id.into(),
                approved,
                reason: reason.into(),
                remember: false,
            })),
            ..Default::default()
        };
        let guard = self.inject_tx.lock().unwrap();
        match guard.as_ref() {
            Some((_gen, tx)) => tx.send(env).map_err(|e| e.to_string()),
            None => Err("no active connection inject channel".to_string()),
        }
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

    /// Send an `AppToolRequest` envelope to the connected daemon. Returns
    /// `Err` if no daemon is currently connected (no inject channel active).
    pub fn send_app_tool_request(
        &self,
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        args_json: impl Into<String>,
        timeout_ms: u32,
    ) -> Result<(), String> {
        let env = Envelope {
            device_id: "mock-hub".into(),
            msg_id: "app-tool-req".into(),
            ts_ms: 0,
            payload: Some(envelope::Payload::AppToolRequest(AppToolRequest {
                tool_call_id: tool_call_id.into(),
                name: name.into(),
                args_json: args_json.into(),
                timeout_ms,
                context_json: String::new(),
            })),
            ..Default::default()
        };
        let guard = self.inject_tx.lock().unwrap();
        match guard.as_ref() {
            Some((_gen, tx)) => tx.send(env).map_err(|e| e.to_string()),
            None => Err("no active connection inject channel".to_string()),
        }
    }

    pub fn send_app_tool_request_with_context(
        &self,
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        args_json: impl Into<String>,
        context_json: impl Into<String>,
        timeout_ms: u32,
    ) -> Result<(), String> {
        let env = Envelope {
            device_id: "mock-hub".into(),
            msg_id: "app-tool-req".into(),
            ts_ms: 0,
            payload: Some(envelope::Payload::AppToolRequest(AppToolRequest {
                tool_call_id: tool_call_id.into(),
                name: name.into(),
                args_json: args_json.into(),
                timeout_ms,
                context_json: context_json.into(),
            })),
            ..Default::default()
        };
        let guard = self.inject_tx.lock().unwrap();
        match guard.as_ref() {
            Some((_gen, tx)) => tx.send(env).map_err(|e| e.to_string()),
            None => Err("no active connection inject channel".to_string()),
        }
    }

    /// Wait until at least `n` `AppToolResponse` envelopes have been received,
    /// then return all of them. Returns `None` on timeout.
    pub async fn wait_for_app_tool_responses(
        &self,
        n: usize,
        timeout: std::time::Duration,
    ) -> Option<Vec<AppToolResponse>> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let responses = self.captured_app_tool_responses();
            if responses.len() >= n {
                return Some(responses);
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }
}

/// Start a mock hub that accepts every Hello and also accepts injected
/// `AppToolRequest` envelopes (via `Mock::send_app_tool_request`).
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
    let app_tool_responses: Arc<Mutex<Vec<AppToolResponse>>> = Arc::new(Mutex::new(Vec::new()));
    let approval_requests: Arc<Mutex<Vec<ApprovalRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let inject_tx: InjectSlot = Arc::new(Mutex::new(None));
    // Monotonically increasing connection generation counter.
    let conn_gen: Arc<std::sync::atomic::AtomicU64> =
        Arc::new(std::sync::atomic::AtomicU64::new(0));

    let heartbeats_for_task = heartbeats.clone();
    let file_responses_for_task = file_responses.clone();
    let app_tools_updates_for_task = app_tools_updates.clone();
    let app_tool_responses_for_task = app_tool_responses.clone();
    let approval_requests_for_task = approval_requests.clone();
    let inject_tx_for_task = inject_tx.clone();
    let conn_gen_for_task = conn_gen.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accept = listener.accept() => {
                    let (stream, _) = match accept {
                        Ok(pair) => pair,
                        Err(_) => break,
                    };
                    let conn_gen_id = conn_gen_for_task
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        + 1;
                    tokio::spawn(handle_conn(
                        stream,
                        behavior.clone(),
                        heartbeats_for_task.clone(),
                        file_responses_for_task.clone(),
                        app_tools_updates_for_task.clone(),
                        app_tool_responses_for_task.clone(),
                        approval_requests_for_task.clone(),
                        inject_tx_for_task.clone(),
                        conn_gen_id,
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
        app_tool_responses,
        approval_requests,
        inject_tx,
        _shutdown: shutdown_tx,
        _task: task,
    }
}

#[allow(clippy::too_many_arguments)] // test mock: capture sinks are individually threaded for clarity
async fn handle_conn(
    stream: tokio::net::TcpStream,
    behavior: Behavior,
    heartbeats: Arc<Mutex<Vec<Heartbeat>>>,
    file_responses: Arc<Mutex<Vec<FileResponse>>>,
    app_tools_updates: Arc<Mutex<Vec<AppToolsUpdate>>>,
    app_tool_responses: Arc<Mutex<Vec<AppToolResponse>>>,
    approval_requests: Arc<Mutex<Vec<ApprovalRequest>>>,
    inject_tx: InjectSlot,
    conn_generation: u64,
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

            // Set up an inject channel so tests can push AppToolRequest
            // envelopes into the active connection via
            // `Mock::send_app_tool_request`.
            let (conn_inject_tx, mut conn_inject_rx) =
                tokio::sync::mpsc::unbounded_channel::<Envelope>();
            *inject_tx.lock().unwrap() = Some((conn_generation, conn_inject_tx));

            // Keep the connection open until the client closes it, and
            // record every `Heartbeat`, `AppToolsUpdate`, and
            // `AppToolResponse` envelope observed on the way.
            // Also forward any injected envelopes (AppToolRequest etc.) to
            // the daemon.
            // If `drop_after_n_app_tools_updates` is set, close the
            // connection after that many AppToolsUpdate messages.
            let mut app_tools_count = 0usize;
            loop {
                tokio::select! {
                    // Inbound from daemon.
                    msg = src.next() => {
                        let m = match msg {
                            Some(Ok(m)) => m,
                            _ => break,
                        };
                        let frame = match m {
                            WsMessage::Binary(bytes) => bytes,
                            WsMessage::Pong(_) => continue,
                            _ => continue,
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
                                if let Some(limit) = drop_after_n_app_tools_updates
                                    && app_tools_count >= limit
                                {
                                    // Send a WS Close frame so the daemon's
                                    // read loop sees a clean close and
                                    // reconnects.
                                    let _ = sink
                                        .send(WsMessage::Close(Some(CloseFrame {
                                            code: CloseCode::Normal,
                                            reason: Cow::Borrowed("test-reconnect"),
                                        })))
                                        .await;
                                    break;
                                }
                            }
                            Some(envelope::Payload::AppToolResponse(resp)) => {
                                app_tool_responses.lock().unwrap().push(resp);
                            }
                            Some(envelope::Payload::ApprovalRequest(req)) => {
                                approval_requests.lock().unwrap().push(req);
                            }
                            _ => {}
                        }
                    }
                    // Inject: test sends an envelope to push to the daemon.
                    inject = conn_inject_rx.recv() => {
                        match inject {
                            Some(env) => {
                                let _ = sink
                                    .send(WsMessage::Binary(env.encode_to_vec()))
                                    .await;
                            }
                            None => break,
                        }
                    }
                }
            }
            // Clear the inject channel only if it still belongs to this
            // connection (guard against stale cleanup clobbering a newer
            // connection's sender when the daemon reconnects).
            let mut guard = inject_tx.lock().unwrap();
            if matches!(guard.as_ref(), Some((stored_gen, _)) if *stored_gen == conn_generation) {
                *guard = None;
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
                    Some(envelope::Payload::AppToolResponse(resp)) => {
                        app_tool_responses.lock().unwrap().push(resp);
                    }
                    _ => {}
                }
            }
        }
    }
}
