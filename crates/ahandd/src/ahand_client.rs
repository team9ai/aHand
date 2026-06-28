use std::sync::{Arc, Mutex};
use std::time::Duration;

use ahand_protocol::{
    BrowserResponse, Envelope, Heartbeat, Hello, HelloAccepted, HelloChallenge, JobFinished,
    JobRejected, envelope, hello,
};
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite;
use tracing::{debug, error, info, warn};

use tokio::sync::watch;

use crate::app_tool_registry::{AppToolInvocation, AppToolRegistry};
use crate::approval::ApprovalManager;
use crate::browser::BrowserManager;
use crate::config::Config;
use crate::device_identity::DeviceIdentity;
use crate::executor::{self, EnvelopeSink as _};
use crate::file_manager::FileManager;
use crate::outbox::{Outbox, prepare_outbound};
use crate::plugin_runtime::{CapabilityKind, CapabilityUnavailable, JobProvider};
use crate::registry::{IsKnown, JobRegistry};
use crate::session::{SessionDecision, SessionManager};
use crate::store::{Direction, RunStore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelloAuthMode {
    Ed25519,
    Bootstrap(String),
}

/// RAII helper that flips a `watch` channel to `true` when dropped. Used by
/// `connect_with_auth` to signal connection close to any detached approval
/// tasks that the dispatch loop spawned, so they can bail out instead of
/// waiting for the ApprovalManager's default 24-hour timeout.
struct CloseGuard {
    tx: watch::Sender<bool>,
}

impl Drop for CloseGuard {
    fn drop(&mut self) {
        let _ = self.tx.send(true);
    }
}

#[derive(Clone)]
struct BufferedEnvelopeSender {
    tx: mpsc::UnboundedSender<OutboundFrame>,
    outbox: Arc<Mutex<Outbox>>,
}

struct QueuedEnvelope {
    frame: Vec<u8>,
    envelope: Envelope,
}

/// Multiplexed sink output. App-level Envelope frames go through the
/// outbox-stamping pipeline; WebSocket-level Ping frames bypass the
/// outbox and only exist to keep the connection liveness-checked from
/// the daemon side. Pings are sent so the read loop sees a Pong (auto-
/// replied by tungstenite per RFC 6455) before its watchdog timeout
/// fires — that's how we detect zombie TCP connections that survived
/// macOS sleep/wake or NAT timeout without any visible socket error.
///
/// `DirectEnvelope` bypasses outbox SEQUENCING/replay (the snapshot is
/// idempotent and re-generated fresh at the start of every new connection),
/// but is still trace-logged to RunStore so the operator can observe it.
/// Encoding happens inside the send task so the unencoded `Envelope` is
/// available for store logging.
#[allow(clippy::large_enum_variant)] // Envelope is the hot path; boxing adds indirection cost
enum OutboundFrame {
    Envelope(QueuedEnvelope),
    WsPing(Vec<u8>),
    DirectEnvelope(Envelope),
}

impl BufferedEnvelopeSender {
    fn new(tx: mpsc::UnboundedSender<OutboundFrame>, outbox: Arc<Mutex<Outbox>>) -> Self {
        Self { tx, outbox }
    }

    /// Send a raw WebSocket Ping. Returns Err only if the receiver task has
    /// already exited (session tearing down).
    fn send_ping(&self, payload: Vec<u8>) -> Result<(), ()> {
        self.tx.send(OutboundFrame::WsPing(payload)).map_err(|_| ())
    }

    /// Send an envelope WITHOUT going through the outbox sequencing/replay
    /// pipeline. Used for idempotent push messages (e.g. `AppToolsUpdate`
    /// snapshots) that must not be replayed on reconnect because a fresh
    /// snapshot is generated at the start of every new connection. The
    /// envelope is still trace-logged to RunStore; encoding happens in the
    /// send task.
    ///
    /// do NOT use for traffic requiring at-least-once delivery — anything
    /// that must survive reconnect goes through send(); direct frames carry
    /// seq=0 and sit outside the ack/dedup window.
    fn send_direct(&self, envelope: Envelope) -> Result<(), ()> {
        self.tx
            .send(OutboundFrame::DirectEnvelope(envelope))
            .map_err(|_| ())
    }
}

impl crate::executor::EnvelopeSink for BufferedEnvelopeSender {
    fn send(&self, mut envelope: Envelope) -> Result<(), ()> {
        let frame = {
            let mut outbox = self.outbox.lock().expect("outbox mutex poisoned");
            prepare_outbound(&mut outbox, &mut envelope)
        };
        self.tx
            .send(OutboundFrame::Envelope(QueuedEnvelope { frame, envelope }))
            .map_err(|_| ())
    }
}

/// Coarse outcome of a single `connect()` attempt, used by library callers
/// that want to observe handshake success/failure without scraping logs.
#[derive(Debug, Clone)]
#[allow(dead_code)] // variant fields carried for future observers / SDK consumers
pub enum ConnectOutcome {
    /// The Hello handshake was accepted.
    HandshakeAccepted,
    /// Every configured auth mode was rejected by the hub.
    HandshakeRejected(String),
    /// Transport-level failure (dial, TLS, malformed frame, …).
    Session(String),
    /// The session completed cleanly (remote closed, etc.).
    Disconnected,
}

/// Sink for [`ConnectOutcome`] events. `run_with_reporter` calls this for
/// every handshake attempt so callers can drive state machines off it.
pub trait ClientReporter: Send + Sync + 'static {
    fn report(&self, outcome: ConnectOutcome);
}

impl<F> ClientReporter for F
where
    F: Fn(ConnectOutcome) + Send + Sync + 'static,
{
    fn report(&self, outcome: ConnectOutcome) {
        (self)(outcome)
    }
}

struct NoopReporter;
impl ClientReporter for NoopReporter {
    fn report(&self, _outcome: ConnectOutcome) {}
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    config: Config,
    device_id: String,
    registry: Arc<JobRegistry>,
    store: Option<Arc<RunStore>>,
    session_mgr: Arc<SessionManager>,
    approval_mgr: Arc<ApprovalManager>,
    approval_broadcast_tx: broadcast::Sender<Envelope>,
    browser_mgr: Arc<BrowserManager>,
    file_mgr: Arc<FileManager>,
    app_tools: Arc<AppToolRegistry>,
) -> anyhow::Result<()> {
    run_with_reporter(
        config,
        device_id,
        registry,
        store,
        session_mgr,
        approval_mgr,
        approval_broadcast_tx,
        browser_mgr,
        file_mgr,
        app_tools,
        Arc::new(NoopReporter),
    )
    .await
}

/// Variant of [`run`] that pushes every handshake outcome into `reporter`.
///
/// Library callers (e.g. `public_api::spawn`) use this to drive a status
/// channel without modifying the reconnect loop.
#[allow(clippy::too_many_arguments)]
pub async fn run_with_reporter(
    config: Config,
    device_id: String,
    registry: Arc<JobRegistry>,
    store: Option<Arc<RunStore>>,
    session_mgr: Arc<SessionManager>,
    approval_mgr: Arc<ApprovalManager>,
    approval_broadcast_tx: broadcast::Sender<Envelope>,
    browser_mgr: Arc<BrowserManager>,
    file_mgr: Arc<FileManager>,
    app_tools: Arc<AppToolRegistry>,
    reporter: Arc<dyn ClientReporter>,
) -> anyhow::Result<()> {
    let hub_config = config.hub_config();
    let identity_path = hub_config
        .private_key_path
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(crate::device_identity::default_identity_path);
    let identity = DeviceIdentity::load_or_create(&identity_path)?;
    let bearer_token = hub_config.bootstrap_token.clone();
    // Prefer millisecond precision when provided (library callers thread
    // a `Duration` through), else fall back to the TOML-friendly
    // `heartbeat_interval_secs`, else default to 60s.
    let heartbeat_interval = match hub_config.heartbeat_interval_ms {
        Some(ms) => Duration::from_millis(ms.max(1)),
        None => Duration::from_secs(hub_config.heartbeat_interval_secs.unwrap_or(60).max(1)),
    };

    // Outbox survives across reconnects.
    let outbox = Arc::new(Mutex::new(Outbox::new(10_000)));

    let mut backoff = 1u64;

    loop {
        info!(url = %config.server_url, "connecting to cloud");

        let attempt = connect_reporting(
            &config.server_url,
            &device_id,
            &identity,
            bearer_token.clone(),
            heartbeat_interval,
            &session_mgr,
            &registry,
            &store,
            &outbox,
            &approval_mgr,
            &approval_broadcast_tx,
            &browser_mgr,
            &file_mgr,
            &app_tools,
            reporter.as_ref(),
        )
        .await;

        match attempt {
            Ok(()) => {
                info!("disconnected from cloud");
                reporter.report(ConnectOutcome::Disconnected);
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
async fn connect_reporting(
    url: &str,
    device_id: &str,
    identity: &DeviceIdentity,
    bearer_token: Option<String>,
    heartbeat_interval: Duration,
    session_mgr: &Arc<SessionManager>,
    registry: &Arc<JobRegistry>,
    store: &Option<Arc<RunStore>>,
    outbox: &Arc<Mutex<Outbox>>,
    approval_mgr: &Arc<ApprovalManager>,
    approval_broadcast_tx: &broadcast::Sender<Envelope>,
    browser_mgr: &Arc<BrowserManager>,
    file_mgr: &Arc<FileManager>,
    app_tools: &Arc<AppToolRegistry>,
    reporter: &dyn ClientReporter,
) -> anyhow::Result<()> {
    let auth_modes = hello_auth_modes(bearer_token.as_deref());
    let mut last_handshake_error = None;

    for auth_mode in auth_modes {
        let outcome = connect_with_auth(
            url,
            device_id,
            identity,
            &auth_mode,
            heartbeat_interval,
            session_mgr,
            registry,
            store,
            outbox,
            approval_mgr,
            approval_broadcast_tx,
            browser_mgr,
            file_mgr,
            app_tools,
            reporter,
        )
        .await;
        match outcome {
            Ok(()) => return Ok(()),
            Err(ConnectError::HandshakeRejected(err)) => {
                warn!(?auth_mode, error = %err, "hello auth rejected");
                last_handshake_error = Some(err);
            }
            Err(ConnectError::Session(err)) => {
                reporter.report(ConnectOutcome::Session(err.to_string()));
                return Err(err);
            }
        }
    }

    let err = last_handshake_error.unwrap_or_else(|| anyhow::anyhow!("device hello rejected"));
    reporter.report(ConnectOutcome::HandshakeRejected(err.to_string()));
    Err(err)
}

#[allow(clippy::too_many_arguments)]
async fn connect_with_auth(
    url: &str,
    device_id: &str,
    identity: &DeviceIdentity,
    auth_mode: &HelloAuthMode,
    heartbeat_interval: Duration,
    session_mgr: &Arc<SessionManager>,
    registry: &Arc<JobRegistry>,
    store: &Option<Arc<RunStore>>,
    outbox: &Arc<Mutex<Outbox>>,
    approval_mgr: &Arc<ApprovalManager>,
    approval_broadcast_tx: &broadcast::Sender<Envelope>,
    browser_mgr: &Arc<BrowserManager>,
    file_mgr: &Arc<FileManager>,
    app_tools: &Arc<AppToolRegistry>,
    reporter: &dyn ClientReporter,
) -> Result<(), ConnectError> {
    // OS-level TCP keepalive is the lower-tier twin of the WS Ping/Pong
    // watchdog: the watchdog catches application-level zombies in
    // 2× heartbeat_interval; TCP keepalive catches OS-level zombies (NAT
    // rewrite, sleep/wake) in ~60s regardless of WS traffic, and is
    // critical on macOS where the default keepalive idle is 2 hours.
    let tcp = connect_tcp_with_keepalive(url)
        .await
        .map_err(ConnectError::Session)?;
    let (ws_stream, _) = tokio_tungstenite::client_async_tls_with_config(url, tcp, None, None)
        .await
        .map_err(anyhow::Error::from)
        .map_err(ConnectError::Session)?;
    let (mut sink, mut stream) = ws_stream.split();

    let challenge = recv_hello_challenge(&mut stream).await?;
    let last_ack = outbox.lock().expect("outbox mutex poisoned").local_ack();
    info!(last_ack, "connected, sending Hello");

    // Send Hello envelope — Hello is NOT stamped (seq=0), it's a connection signal.
    let capability_router = crate::plugin_runtime::build_router(browser_mgr, file_mgr)
        .await
        .map_err(ConnectError::Session)?;
    let hello = build_hello_envelope_with_capabilities(
        device_id,
        identity,
        last_ack,
        capability_router
            .active_wire_capabilities()
            .into_iter()
            .map(str::to_string)
            .collect(),
        &challenge.nonce,
        match auth_mode {
            HelloAuthMode::Ed25519 => None,
            HelloAuthMode::Bootstrap(token) => Some(token.clone()),
        },
    );
    let data = hello.encode_to_vec();
    if let Some(s) = store {
        s.log_envelope(&hello, Direction::Outbound).await;
    }
    sink.send(tungstenite::Message::Binary(data))
        .await
        .map_err(anyhow::Error::from)
        .map_err(ConnectError::Session)?;
    let accepted = recv_hello_accepted(&mut stream).await?;
    info!(auth_method = %accepted.auth_method, "hello accepted");
    reporter.report(ConnectOutcome::HandshakeAccepted);

    // Replay unacked messages from previous connection.
    let unacked = outbox
        .lock()
        .expect("outbox mutex poisoned")
        .drain_unacked();
    if !unacked.is_empty() {
        info!(count = unacked.len(), "replaying unacked messages");
        for data in unacked {
            sink.send(tungstenite::Message::Binary(data))
                .await
                .map_err(anyhow::Error::from)
                .map_err(ConnectError::Session)?;
        }
    }

    // Channel: executor sends Envelope objects, send task stamps + encodes + sends.
    // Multiplexed with WS-level Ping frames so the same task is the sole writer
    // to `sink` (avoids needing a Mutex around the sink).
    let (raw_tx, mut rx) = mpsc::unbounded_channel::<OutboundFrame>();
    let tx = BufferedEnvelopeSender::new(raw_tx, Arc::clone(outbox));
    let store_send = store.clone();

    // R20: a watch channel signals connection close to detached approval
    // tasks spawned by handle_file_request. When connect_with_auth returns
    // (normal or error), the guard is dropped and close_rx.changed() fires,
    // letting pending approval waiters bail out immediately instead of
    // waiting up to 24 hours for the approval-mgr timeout.
    let (close_tx, close_rx) = watch::channel(false);
    let _close_guard = CloseGuard { tx: close_tx };

    if let Some(suggestion) = accepted.update_suggestion {
        info!(update_id = %suggestion.update_id, target = %suggestion.target_version,
            "hub suggests update during registration");
        let params = crate::updater::UpdateParams::from(suggestion);
        crate::updater::spawn_update(params, device_id.to_string(), tx.clone());
    }

    // AppToolsUpdate advertising:
    //
    // Subscribe to revision changes BEFORE sending the initial snapshot so
    // that any registration that races the post-handshake window (i.e. a
    // revision bump between subscribe and the initial send) is captured by
    // the watcher task and not silently dropped.
    //
    // Ordering:
    //   1. subscribe_revision()          → get a Receiver
    //   2. borrow_and_update()           → mark current revision as "seen"
    //   3. send initial snapshot         → covers any tools registered before connect
    //   4. spawn watcher task            → fires on every subsequent bump
    //
    // Any revision bump after step 2 wakes the watcher (step 4) and
    // triggers a fresh snapshot — no duplicate and no missed update.
    {
        let mut revision_rx = app_tools.subscribe_revision();
        // Mark current revision as seen so the watcher doesn't re-send the
        // initial snapshot.
        revision_rx.borrow_and_update();

        // Send the initial snapshot (covers all tools registered before or
        // during the handshake window). Use send_direct so this envelope is
        // NOT added to the outbox — snapshots are idempotent and must not be
        // replayed on reconnect (a fresh snapshot is generated each time).
        let initial_snap = app_tools.snapshot().await;
        debug!(
            revision = initial_snap.revision,
            tool_count = initial_snap.tools.len(),
            "advertising app tools snapshot"
        );
        let _ = tx.send_direct(app_tools_snapshot_envelope(device_id, initial_snap));

        // Spawn a connection-scoped watcher that re-sends the snapshot on
        // every registry mutation.  Exits when `close_tx` fires (connection
        // teardown) or when `tx` is dropped (mpsc closes).
        let watcher_tx = tx.clone();
        let watcher_app_tools = Arc::clone(app_tools);
        let watcher_device_id = device_id.to_string();
        let mut watcher_close_rx = close_rx.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = watcher_close_rx.changed() => {
                        debug!("app-tools watcher exiting");
                        break;
                    }
                    changed = revision_rx.changed() => {
                        if changed.is_err() {
                            debug!("app-tools watcher exiting");
                            break;
                        }
                        let snap = watcher_app_tools.snapshot().await;
                        debug!(revision = snap.revision, tool_count = snap.tools.len(), "advertising app tools snapshot");
                        // send_direct: snapshots are idempotent, must not be
                        // added to the outbox for replay on reconnect.
                        if watcher_tx.send_direct(app_tools_snapshot_envelope(&watcher_device_id, snap)).is_err() {
                            debug!("app-tools watcher exiting");
                            break;
                        }
                    }
                }
            }
        });
    }

    // Task: receive OutboundFrame from executors + ws-ping task, stamp + encode
    // + send over WS. Multiplexed so the sink stays single-owner.
    let send_handle = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            let msg = match frame {
                OutboundFrame::Envelope(queued) => {
                    // Log outbound envelopes to trace.
                    if let Some(s) = &store_send {
                        s.log_envelope(&queued.envelope, Direction::Outbound).await;
                    }
                    tungstenite::Message::Binary(queued.frame)
                }
                OutboundFrame::WsPing(payload) => tungstenite::Message::Ping(payload),
                // Direct envelopes (e.g. AppToolsUpdate snapshots) bypass
                // outbox sequencing/replay but are still trace-logged.
                OutboundFrame::DirectEnvelope(envelope) => {
                    if let Some(s) = &store_send {
                        s.log_envelope(&envelope, Direction::Outbound).await;
                    }
                    tungstenite::Message::Binary(envelope.encode_to_vec())
                }
            };
            if sink.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Task: periodic Heartbeat envelopes over the buffered sender. Exits when
    // `tx` is dropped (session end) because `BufferedEnvelopeSender::send`
    // fails once the underlying mpsc closes — that is the only termination
    // path and happens cooperatively without a separate shutdown signal.
    let heartbeat_sender = tx.clone();
    let heartbeat_device_id = device_id.to_string();
    let daemon_version = env!("CARGO_PKG_VERSION").to_string();
    let heartbeat_task = spawn_heartbeat_task(
        heartbeat_sender,
        heartbeat_device_id,
        daemon_version,
        heartbeat_interval,
    );

    // Task: WS-level Ping every `heartbeat_interval`. Tungstenite-spec-compliant
    // peers (including our hub) auto-reply with Pong, so the read loop's
    // watchdog timeout (2× heartbeat_interval) is reset on every successful
    // ping. The reason this exists alongside the app-level heartbeat: the
    // app-level heartbeat is a fire-and-forget Binary frame, so a zombie
    // TCP connection (Mac sleep/wake, NAT timeout) accepts the write into
    // the OS send buffer without ever delivering the bytes — daemon never
    // notices, hub eventually marks device offline, UI stays "Online" until
    // the OS gives up on the dead socket (~hours). WS Ping/Pong forces a
    // round-trip — no Pong → no inbound message → watchdog fires → reconnect.
    #[cfg(not(feature = "disable-ws-ping"))]
    let ws_ping_task = {
        let ws_ping_tx = tx.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(heartbeat_interval);
            // Skip first tick so it doesn't race with the Hello handshake.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if ws_ping_tx
                    .send_ping(now_ms().to_be_bytes().to_vec())
                    .is_err()
                {
                    break;
                }
            }
        })
    };
    // Dev-only: disable the WS Ping path so OS-level TCP keepalive is the
    // sole liveness signal. Used by the manual smoke test that verifies
    // the kernel-level fallback recovers a zombie connection on its own.
    #[cfg(feature = "disable-ws-ping")]
    let ws_ping_task: tokio::task::JoinHandle<()> = tokio::spawn(async {});

    let caller_uid = "cloud";

    // Register the cloud caller so session queries return it.
    session_mgr.register_caller(caller_uid).await;

    // Watchdog: if no inbound activity (Pong, app message, anything) for
    // 2× heartbeat_interval, we're talking to a zombie connection. Break
    // out so the outer reconnect loop can dial a fresh socket.
    let read_timeout = heartbeat_interval.saturating_mul(2);

    // Process incoming messages.
    loop {
        let msg = match tokio::time::timeout(read_timeout, stream.next()).await {
            Ok(Some(m)) => m,
            Ok(None) => break, // stream ended
            Err(_) => {
                warn!(
                    timeout_secs = read_timeout.as_secs(),
                    "no inbound activity (no Pong, no message) — closing zombie connection",
                );
                break;
            }
        };
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
            let mut ob = outbox.lock().expect("outbox mutex poisoned");
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
                    browser_mgr,
                    file_mgr,
                )
                .await;
            }
            Some(envelope::Payload::CancelJob(cancel)) => {
                info!(job_id = %cancel.job_id, "received cancel request");
                registry.cancel(&cancel.job_id).await;
            }
            Some(envelope::Payload::ApprovalResponse(resp)) => {
                info!(job_id = %resp.job_id, approved = resp.approved, "received approval response from cloud");
                crate::approval::apply_approval_response(
                    approval_mgr,
                    session_mgr,
                    &resp,
                    caller_uid,
                )
                .await;
            }
            Some(envelope::Payload::SetSessionMode(msg)) => {
                handle_set_session_mode(device_id, session_mgr, &msg, &tx).await;
            }
            Some(envelope::Payload::SessionQuery(query)) => {
                handle_session_query(device_id, session_mgr, &query, &tx).await;
            }
            Some(envelope::Payload::BrowserRequest(req)) => {
                handle_browser_request(
                    device_id,
                    caller_uid,
                    &req,
                    &tx,
                    session_mgr,
                    browser_mgr,
                    file_mgr,
                )
                .await;
            }
            Some(envelope::Payload::FileRequest(req)) => {
                handle_file_request(
                    device_id,
                    caller_uid,
                    req,
                    &tx,
                    session_mgr,
                    browser_mgr,
                    file_mgr,
                    approval_mgr,
                    approval_broadcast_tx,
                    &close_rx,
                )
                .await;
            }
            Some(envelope::Payload::AppToolRequest(req)) => {
                handle_app_tool_request(
                    device_id,
                    caller_uid,
                    req,
                    &tx,
                    session_mgr,
                    approval_mgr,
                    approval_broadcast_tx,
                    app_tools,
                )
                .await;
            }
            Some(envelope::Payload::UpdateCommand(cmd)) => {
                info!(update_id = %cmd.update_id, target = %cmd.target_version,
                    "received update command from hub");
                let params = crate::updater::UpdateParams::from(cmd);
                if !crate::updater::spawn_update(params, device_id.to_string(), tx.clone()) {
                    let _ = tx.send(Envelope {
                        device_id: device_id.to_string(),
                        msg_id: format!("update-reject-{}", device_id),
                        payload: Some(envelope::Payload::UpdateStatus(
                            ahand_protocol::UpdateStatus {
                                update_id: String::new(),
                                state: ahand_protocol::UpdateState::Failed as i32,
                                current_version: env!("CARGO_PKG_VERSION").into(),
                                target_version: String::new(),
                                progress: 0,
                                error: "another update is already in progress".into(),
                            },
                        )),
                        ..Default::default()
                    });
                }
            }
            Some(envelope::Payload::StdinChunk(chunk)) => {
                use crate::executor::StdinInput;
                registry
                    .send_stdin(&chunk.job_id, StdinInput::Data(chunk.data))
                    .await;
            }
            Some(envelope::Payload::TerminalResize(resize)) => {
                use crate::executor::StdinInput;
                registry
                    .send_stdin(
                        &resize.job_id,
                        StdinInput::Resize {
                            cols: resize.cols as u16,
                            rows: resize.rows as u16,
                        },
                    )
                    .await;
            }
            _ => {}
        }
    }

    // Teardown order matters here. Four independent things still hold
    // sender clones and could wedge `send_handle.await`:
    //
    // 1. Detached file-approval tasks spawned by handle_file_request hold
    //    a `tx_clone` AND wait on `close_rx`. They only release their
    //    clone once `close_tx` fires — which CloseGuard does on drop.
    //    Without dropping the guard FIRST, the task would block until
    //    the ApprovalManager 24h timeout and wedge teardown for a day.
    //    (C1 round-3 fix.)
    // 2. The heartbeat task holds its own clone of
    //    `BufferedEnvelopeSender` — if we dropped `tx` first, the send
    //    task's mpsc receiver would still see a live sender and block on
    //    `rx.recv()` indefinitely until the WS broke naturally. Aborting
    //    the task drops its clone.
    // 3. Same logic for `ws_ping_task`.
    // 4. The app-tools watcher task holds `watcher_tx` (a clone of `tx`)
    //    and exits via `watcher_close_rx.changed()`. Dropping `_close_guard`
    //    fires `close_tx`, which wakes the watcher so it releases its clone
    //    before `send_handle.await` is reached. The guard drop MUST precede
    //    `send_handle.await` for this reason.
    drop(_close_guard);
    heartbeat_task.abort();
    let _ = heartbeat_task.await;
    ws_ping_task.abort();
    let _ = ws_ping_task.await;
    drop(tx);
    let _ = send_handle.await;

    Ok(())
}

/// Spawn the heartbeat-emission task.
///
/// Exits cleanly via one of:
///   * `heartbeat_sender.send(...)` returns `Err` (underlying mpsc closed)
///   * the returned `JoinHandle` is aborted by the caller at connection tear-down
fn spawn_heartbeat_task<S>(
    heartbeat_sender: S,
    device_id: String,
    daemon_version: String,
    interval: Duration,
) -> tokio::task::JoinHandle<()>
where
    S: crate::executor::EnvelopeSink + 'static,
{
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // `interval` fires immediately on the first tick; skip it so the
        // first heartbeat lands `interval` after connection open rather
        // than racing with the Hello handshake.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let envelope = Envelope {
                device_id: device_id.clone(),
                msg_id: new_msg_id(),
                ts_ms: now_ms(),
                payload: Some(envelope::Payload::Heartbeat(Heartbeat {
                    sent_at_ms: now_ms(),
                    daemon_version: daemon_version.clone(),
                })),
                ..Default::default()
            };
            if heartbeat_sender.send(envelope).is_err() {
                // Sender closed → session tore down. Exit cleanly so the
                // JoinHandle resolves without the parent needing to abort.
                break;
            }
        }
    })
}

/// Dial a TcpStream to the host:port from `url_str` and apply OS-level
/// keepalive: 30s idle, 10s probe interval, 3 retries (≈60s to detect a
/// dead peer). On macOS the kernel default is 2h idle, so without this
/// the daemon would stay attached to a zombie socket for hours after a
/// laptop sleep/wake even though the WS Ping watchdog has already fired.
///
/// Returns the tokio TcpStream so the caller can layer WebSocket (and
/// optional TLS) on top via `client_async_tls_with_config`.
async fn connect_tcp_with_keepalive(url_str: &str) -> anyhow::Result<tokio::net::TcpStream> {
    use anyhow::Context;
    let parsed = url::Url::parse(url_str).context("invalid websocket url")?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("websocket url missing host: {url_str}"))?;
    let port = parsed.port().unwrap_or_else(|| match parsed.scheme() {
        "wss" => 443,
        _ => 80,
    });
    let addr = format!("{host}:{port}");
    let stream = tokio::net::TcpStream::connect(&addr)
        .await
        .with_context(|| format!("failed to dial {addr}"))?;
    apply_tcp_keepalive(&stream).context("set TCP keepalive options")?;
    Ok(stream)
}

/// Apply the canonical keepalive parameters to an already-connected TCP
/// socket: 30s idle, 10s probe interval, 3 retries (~60s detection).
/// Pulled out so unit tests can construct an arbitrary TcpStream
/// (e.g. against a `TcpListener` on 127.0.0.1) and assert the options
/// were set without going through the full WS handshake.
///
/// `with_retries` (TCP_KEEPCNT) is missing from socket2 0.5 on macOS, so
/// we fall back to raw setsockopt there. Without it macOS would default
/// to 8 retries (~110s detection), exceeding the 90s budget the WS
/// reconnect path is built around.
fn apply_tcp_keepalive(stream: &tokio::net::TcpStream) -> std::io::Result<()> {
    #[allow(unused_mut)]
    let mut keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(30))
        .with_interval(Duration::from_secs(10));
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    {
        keepalive = keepalive.with_retries(3);
    }
    socket2::SockRef::from(stream).set_tcp_keepalive(&keepalive)?;

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    set_tcp_keepcnt_macos(stream, 3)?;

    Ok(())
}

/// macOS / iOS path for `TCP_KEEPCNT`. socket2 0.5 doesn't expose this
/// option for Apple targets, but the kernel honors it via raw
/// setsockopt(IPPROTO_TCP, TCP_KEEPCNT). Returns the underlying io error
/// so the caller's context can attach to it cleanly.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn set_tcp_keepcnt_macos(stream: &tokio::net::TcpStream, count: u32) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let value: libc::c_int = count as libc::c_int;
    // SAFETY: `fd` is owned by `stream` for the duration of this call;
    // `&value` lives across the syscall; size matches `c_int`.
    let ret = unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::IPPROTO_TCP,
            libc::TCP_KEEPCNT,
            &value as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn app_tools_snapshot_envelope(
    device_id: &str,
    snapshot: ahand_protocol::AppToolsUpdate,
) -> Envelope {
    Envelope {
        device_id: device_id.to_string(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::AppToolsUpdate(snapshot)),
        ..Default::default()
    }
}

// Bin target never calls this directly; exercised via lib integration tests
// (hub_handshake) and external SDK consumers.
#[allow(dead_code)]
pub fn build_hello_envelope(
    device_id: &str,
    identity: &DeviceIdentity,
    last_ack: u64,
    browser_enabled: bool,
    file_enabled: bool,
    challenge_nonce: &[u8],
    bearer_token: Option<String>,
) -> Envelope {
    let mut capabilities = vec!["exec".to_string()];
    if browser_enabled {
        // Device-reported capability name binds to the concrete
        // implementation. Format: `browser-<backend>`. Currently only
        // playwright-cli is supported. A future non-playwright backend
        // (e.g. native WebView, chromedp) would report `browser-<that>`
        // instead; worker-side `deriveCaps` in team9-agent-pi maps all
        // legacy / future variants to the same HostCapability.
        capabilities.push("browser-playwright-cli".to_string());
    }
    if file_enabled {
        capabilities.push("file".to_string());
    }

    build_hello_envelope_with_capabilities(
        device_id,
        identity,
        last_ack,
        capabilities,
        challenge_nonce,
        bearer_token,
    )
}

pub fn build_hello_envelope_with_capabilities(
    device_id: &str,
    identity: &DeviceIdentity,
    last_ack: u64,
    capabilities: Vec<String>,
    challenge_nonce: &[u8],
    bearer_token: Option<String>,
) -> Envelope {
    let signed_at_ms = identity.next_hello_signed_at_ms();
    let mut hello = Hello {
        version: env!("CARGO_PKG_VERSION").to_string(),
        hostname: gethostname::gethostname().to_string_lossy().to_string(),
        os: std::env::consts::OS.to_string(),
        capabilities: hello_capabilities_from_wire_names(capabilities),
        last_ack,
        auth: None,
    };

    let signature = identity.sign_hello(device_id, &hello, signed_at_ms, challenge_nonce);
    hello.auth = if let Some(token) = bearer_token {
        Some(hello::Auth::Bootstrap(ahand_protocol::BootstrapAuth {
            bearer_token: token,
            public_key: identity.public_key_bytes(),
            signature,
            signed_at_ms,
        }))
    } else {
        Some(hello::Auth::Ed25519(ahand_protocol::Ed25519Auth {
            public_key: identity.public_key_bytes(),
            signature,
            signed_at_ms,
        }))
    };

    Envelope {
        device_id: device_id.to_string(),
        msg_id: "hello-0".to_string(),
        ts_ms: signed_at_ms,
        payload: Some(envelope::Payload::Hello(hello)),
        ..Default::default()
    }
}

fn hello_capabilities_from_wire_names(capabilities: Vec<String>) -> Vec<String> {
    capabilities
}

pub fn hello_auth_modes(bootstrap_token: Option<&str>) -> Vec<HelloAuthMode> {
    let mut modes = vec![HelloAuthMode::Ed25519];
    if let Some(token) = bootstrap_token {
        modes.push(HelloAuthMode::Bootstrap(token.to_owned()));
    }
    modes
}

fn reject_job_for_capability_error<T>(
    device_id: &str,
    req: &ahand_protocol::JobRequest,
    tx: &T,
    reason: String,
) where
    T: crate::executor::EnvelopeSink,
{
    warn!(
        job_id = %req.job_id,
        tool = %req.tool,
        reason = %reason,
        "job rejected by capability router"
    );
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

fn file_unavailable_response(
    req: &ahand_protocol::FileRequest,
    path: &str,
    unavailable: &CapabilityUnavailable,
) -> ahand_protocol::FileResponse {
    crate::file_manager::error_response(
        req.request_id.clone(),
        ahand_protocol::FileErrorCode::PolicyDenied,
        path,
        &unavailable.to_protocol_message(),
    )
}

fn managed_runtime_interactive_rejection(
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

/// Handle an incoming JobRequest with idempotency + session mode check.
#[allow(clippy::too_many_arguments)]
async fn handle_job_request<T>(
    req: ahand_protocol::JobRequest,
    device_id: &str,
    caller_uid: &str,
    tx: &T,
    session_mgr: &Arc<SessionManager>,
    registry: &Arc<JobRegistry>,
    store: &Option<Arc<RunStore>>,
    approval_mgr: &Arc<ApprovalManager>,
    approval_broadcast_tx: &broadcast::Sender<Envelope>,
    browser_mgr: &Arc<BrowserManager>,
    file_mgr: &Arc<FileManager>,
) where
    T: crate::executor::EnvelopeSink,
{
    let provider_registry =
        match crate::plugin_runtime::build_provider_registry(browser_mgr, file_mgr).await {
            Ok(registry) => registry,
            Err(err) => {
                reject_job_for_capability_error(
                    device_id,
                    &req,
                    tx,
                    format!("exec capability unavailable: failed to inspect host resources: {err}"),
                );
                return;
            }
        };
    let job_provider = match provider_registry.resolve_job_provider(&req.tool) {
        Ok(provider) => provider,
        Err(err) => {
            reject_job_for_capability_error(device_id, &req, tx, err.to_protocol_message());
            return;
        }
    };
    if req.interactive && matches!(job_provider, JobProvider::ManagedRuntime { .. }) {
        let _ = tx.send(managed_runtime_interactive_rejection(device_id, &req));
        return;
    }

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
            spawn_job(device_id, req, job_provider, tx, registry, store).await;
        }
        SessionDecision::NeedsApproval {
            reason,
            previous_refusals,
        } => {
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
            let tx_clone = (*tx).clone();
            let did = device_id.to_string();
            let reg = Arc::clone(registry);
            let st = store.clone();
            let amgr = Arc::clone(approval_mgr);
            let smgr = Arc::clone(session_mgr);
            let timeout = amgr.default_timeout();
            let job_id = req.job_id.clone();
            let cuid = caller_uid.to_string();
            let job_provider = job_provider.clone();

            tokio::spawn(async move {
                let result = tokio::time::timeout(timeout, approval_rx).await;
                match result {
                    Ok(Ok(resp)) if resp.approved => {
                        info!(job_id = %job_id, "approval granted");
                        spawn_job(&did, req, job_provider, &tx_clone, &reg, &st).await;
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
async fn spawn_job<T>(
    device_id: &str,
    req: ahand_protocol::JobRequest,
    provider: JobProvider,
    tx: &T,
    registry: &Arc<JobRegistry>,
    store: &Option<Arc<RunStore>>,
) where
    T: crate::executor::EnvelopeSink,
{
    let job_id = req.job_id.clone();
    let tx_clone = (*tx).clone();
    let did = device_id.to_string();
    let reg = Arc::clone(registry);
    let st = store.clone();
    let interactive = req.interactive;

    let (cancel_tx, cancel_rx) = mpsc::channel(1);

    if interactive {
        if matches!(provider, JobProvider::ManagedRuntime { .. }) {
            let _ = tx.send(managed_runtime_interactive_rejection(device_id, &req));
            return;
        }

        let (stdin_tx, stdin_rx) = mpsc::unbounded_channel::<executor::StdinInput>();
        reg.register_interactive(job_id.clone(), cancel_tx, stdin_tx)
            .await;

        let active = reg.active_count().await;
        info!(job_id = %job_id, active_jobs = active, interactive = true, "interactive job accepted, acquiring permit");

        tokio::spawn(async move {
            let _permit = reg.acquire_permit().await;
            let (exit_code, error) =
                executor::run_job_pty(did, req, tx_clone, cancel_rx, stdin_rx, st).await;
            reg.remove(&job_id).await;
            reg.mark_completed(job_id, exit_code, error).await;
        });
    } else {
        reg.register(job_id.clone(), cancel_tx).await;

        let active = reg.active_count().await;
        info!(job_id = %job_id, active_jobs = active, "job accepted, acquiring permit");

        tokio::spawn(async move {
            let _permit = reg.acquire_permit().await;
            let (exit_code, error) = match provider {
                JobProvider::DefaultExec => {
                    executor::run_job(did, req, tx_clone, cancel_rx, st).await
                }
                JobProvider::ManagedRuntime { target, .. } => {
                    executor::run_job_with_target(did, req, target, tx_clone, cancel_rx, st).await
                }
            };
            reg.remove(&job_id).await;
            reg.mark_completed(job_id, exit_code, error).await;
        });
    }
}

async fn handle_set_session_mode<T>(
    device_id: &str,
    session_mgr: &Arc<SessionManager>,
    msg: &ahand_protocol::SetSessionMode,
    tx: &T,
) where
    T: crate::executor::EnvelopeSink,
{
    let mode = ahand_protocol::SessionMode::try_from(msg.mode)
        .unwrap_or(ahand_protocol::SessionMode::Inactive);
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

async fn handle_session_query<T>(
    device_id: &str,
    session_mgr: &Arc<SessionManager>,
    query: &ahand_protocol::SessionQuery,
    tx: &T,
) where
    T: crate::executor::EnvelopeSink,
{
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

async fn handle_browser_request<T>(
    device_id: &str,
    caller_uid: &str,
    req: &ahand_protocol::BrowserRequest,
    tx: &T,
    session_mgr: &Arc<SessionManager>,
    browser_mgr: &Arc<BrowserManager>,
    file_mgr: &Arc<FileManager>,
) where
    T: crate::executor::EnvelopeSink,
{
    info!(
        request_id = %req.request_id,
        session_id = %req.session_id,
        action = %req.action,
        "received browser request"
    );

    let provider_registry = match crate::plugin_runtime::build_provider_registry(
        browser_mgr,
        file_mgr,
    )
    .await
    {
        Ok(registry) => registry,
        Err(err) => {
            let resp_env = Envelope {
                device_id: device_id.to_string(),
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
            return;
        }
    };
    if let Err(unavailable) = provider_registry.ensure(CapabilityKind::Browser) {
        let resp_env = Envelope {
            device_id: device_id.to_string(),
            msg_id: new_msg_id(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::BrowserResponse(
                browser_unavailable_response(req, &unavailable),
            )),
            ..Default::default()
        };
        let _ = tx.send(resp_env);
        return;
    }

    // Session mode check using a synthetic JobRequest.
    // `tool: "browser"` here is the proto field that routes the request
    // to the daemon's browser handler. It is NOT the same as the
    // device-advertised capability string (see the block above where we
    // push "browser-playwright-cli"). This proto field stays unchanged
    // for wire-compat with the deprecated /api/control/browser endpoint;
    // see that endpoint's module-level deprecation banner.
    let synthetic_req = ahand_protocol::JobRequest {
        job_id: req.request_id.clone(),
        tool: "browser".to_string(),
        args: vec![req.action.clone()],
        ..Default::default()
    };

    match session_mgr.check(&synthetic_req, caller_uid).await {
        crate::session::SessionDecision::Deny(reason) => {
            warn!(request_id = %req.request_id, reason = %reason, "browser request rejected by session mode");
            let resp_env = Envelope {
                device_id: device_id.to_string(),
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
        }
        crate::session::SessionDecision::Allow
        | crate::session::SessionDecision::NeedsApproval { .. } => {
            // Domain check.
            if let Err(reason) = browser_mgr.check_domain(&req.action, &req.params_json) {
                let resp_env = Envelope {
                    device_id: device_id.to_string(),
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
                return;
            }

            // Execute browser command.
            let result = browser_mgr
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

            // Release session tracking on close.
            if req.action == "close" {
                browser_mgr.release_session(&req.session_id).await;
            }

            let resp_env = Envelope {
                device_id: device_id.to_string(),
                msg_id: new_msg_id(),
                ts_ms: now_ms(),
                payload: Some(envelope::Payload::BrowserResponse(resp)),
                ..Default::default()
            };
            let _ = tx.send(resp_env);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_file_request<T>(
    device_id: &str,
    caller_uid: &str,
    req: ahand_protocol::FileRequest,
    tx: &T,
    session_mgr: &Arc<SessionManager>,
    browser_mgr: &Arc<BrowserManager>,
    file_mgr: &Arc<FileManager>,
    approval_mgr: &Arc<ApprovalManager>,
    approval_broadcast_tx: &broadcast::Sender<Envelope>,
    close_rx: &watch::Receiver<bool>,
) where
    T: crate::executor::EnvelopeSink,
{
    info!(
        request_id = %req.request_id,
        operation = ?req.operation.as_ref().map(file_op_name),
        "received file request"
    );

    let send_file_response = |resp: ahand_protocol::FileResponse| {
        let env = Envelope {
            device_id: device_id.to_string(),
            msg_id: new_msg_id(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::FileResponse(resp)),
            ..Default::default()
        };
        let _ = tx.send(env);
    };

    // Derive a single string identifying the path(s) this request
    // touches, for inclusion in any synthetic FileError.path we build
    // at this layer. file_mgr.request_paths(&req) returns one entry
    // per operand (Move/Copy/CreateSymlink → two, single-path ops →
    // one); join with comma so the operator can see exactly which
    // file the daemon refused to act on, instead of a blank `path`
    // field hiding behind a generic "PolicyDenied" message.
    let req_paths_joined = file_mgr.request_paths(&req).join(", ");

    let provider_registry =
        match crate::plugin_runtime::build_provider_registry(browser_mgr, file_mgr).await {
            Ok(registry) => registry,
            Err(err) => {
                send_file_response(crate::file_manager::error_response(
                    req.request_id.clone(),
                    ahand_protocol::FileErrorCode::PolicyDenied,
                    &req_paths_joined,
                    &format!(
                        "file capability unavailable: failed to inspect host resources: {err}"
                    ),
                ));
                return;
            }
        };
    if let Err(unavailable) = provider_registry.ensure(CapabilityKind::File) {
        send_file_response(file_unavailable_response(
            &req,
            &req_paths_joined,
            &unavailable,
        ));
        return;
    }

    // Pre-flight policy check — runs the same allowlist/denylist checks that
    // dispatch would, but also surfaces `dangerous_paths` hits as
    // "needs_approval". If any path is outright denied, short-circuit with
    // the FileError. Otherwise we carry the structured `policy_escalation`
    // forward so the session-mode branch below can escalate to approval —
    // and use the specific reason from policy (which path / why) rather
    // than a generic "dangerous_paths" string.
    let policy_escalation = match file_mgr.check_request_approval(&req).await {
        Ok(escalation) => escalation,
        Err(err) => {
            warn!(request_id = %req.request_id, code = err.code, "file request denied by policy pre-check");
            send_file_response(ahand_protocol::FileResponse {
                request_id: req.request_id.clone(),
                result: Some(ahand_protocol::file_response::Result::Error(err)),
            });
            return;
        }
    };

    // Session mode check — file operations always go through the same gate as
    // other cloud-initiated tools. We synthesise a JobRequest with
    // tool="file" so that the session manager can apply the caller's current
    // policy.
    //
    // R6 — file approvals get a dedicated `file-req:` job_id namespace so
    // a file-request approval can never accidentally evict a real job's
    // pending approval (or vice versa). Previously both shared the
    // caller-chosen request_id/job_id and could collide inside
    // ApprovalManager.
    //
    // R8 — include the paths the request touches in `args` so the user
    // sees what's actually being approved in the UI, not just `tool=file`
    // with `args=[op_name]`.
    let op_name = req
        .operation
        .as_ref()
        .map(file_op_name)
        .unwrap_or("unspecified")
        .to_string();
    let approval_job_id = format!("file-req:{}", req.request_id);
    let mut approval_args = vec![op_name];
    approval_args.extend(file_mgr.request_paths(&req));
    let synthetic_req = ahand_protocol::JobRequest {
        job_id: approval_job_id.clone(),
        tool: "file".to_string(),
        args: approval_args,
        ..Default::default()
    };

    let session_decision = session_mgr.check(&synthetic_req, caller_uid).await;

    let (approval_reason, previous_refusals) = match session_decision {
        SessionDecision::Deny(reason) => {
            warn!(request_id = %req.request_id, reason = %reason, "file request denied by session mode");
            send_file_response(crate::file_manager::error_response(
                req.request_id.clone(),
                ahand_protocol::FileErrorCode::PolicyDenied,
                &req_paths_joined,
                &reason,
            ));
            return;
        }
        SessionDecision::Allow if policy_escalation.is_none() => {
            // Fast path — nothing dangerous, session would allow.
            let response = file_mgr.handle(&req).await;
            send_file_response(response);
            return;
        }
        SessionDecision::Allow => {
            // Session would Allow, but policy pre-check flagged the
            // request (dangerous path, recursive permanent delete, glob
            // scan cap hit, etc.). Force the approval flow with the
            // specific reason policy gave us — operators see e.g.
            // "path '/etc/passwd' is listed in dangerous_paths" rather
            // than a generic "path is listed in dangerous_paths" line.
            let escalation = policy_escalation
                .as_ref()
                .expect("guarded by SessionDecision::Allow if policy_escalation.is_none()");
            info!(
                request_id = %req.request_id,
                kind = ?escalation.kind,
                reason = %escalation.reason,
                "file request escalated to approval by policy pre-check"
            );
            (escalation.reason.clone(), Vec::new())
        }
        SessionDecision::NeedsApproval {
            reason,
            previous_refusals,
        } => {
            info!(
                request_id = %req.request_id,
                reason = %reason,
                "file request needs approval (strict mode)"
            );
            (reason, previous_refusals)
        }
    };

    // Both the Allow+dangerous and NeedsApproval branches fall through here.
    let (approval_req, approval_rx) = approval_mgr
        .submit(
            synthetic_req,
            caller_uid,
            approval_reason,
            previous_refusals,
        )
        .await;

    // Send ApprovalRequest to cloud via WS and broadcast to IPC clients.
    let approval_env = Envelope {
        device_id: device_id.to_string(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::ApprovalRequest(approval_req.clone())),
        ..Default::default()
    };
    let _ = tx.send(approval_env.clone());
    let _ = approval_broadcast_tx.send(approval_env);

    // Wait for the approval response (or timeout, or connection close) in a
    // detached task so the dispatch loop keeps draining inbound frames
    // while approval is pending. R20: we select on close_rx so the task
    // exits promptly when the parent WS connection tears down, instead of
    // lingering until the 24-hour approval timeout.
    let tx_clone = (*tx).clone();
    let file_mgr_clone = Arc::clone(file_mgr);
    let amgr = Arc::clone(approval_mgr);
    let did = device_id.to_string();
    let timeout = amgr.default_timeout();
    // The approval manager key is the namespaced job_id (`file-req:<rid>`),
    // but every FileResponse we emit back to the caller still uses the
    // bare request_id so the HTTP layer can correlate.
    let response_request_id = req.request_id.clone();
    let approval_key = approval_job_id;
    let mut close_rx_clone = close_rx.clone();
    // Clone the joined paths so the spawned task can populate
    // FileError.path on the Denied / TimedOut branches — without it
    // the operator only sees a generic "approval denied" / "approval
    // timed out" with no indication of *which* file. Same observability
    // pattern Copilot caught one layer down in text/binary readers.
    let req_paths_joined_for_task = req_paths_joined.clone();

    tokio::spawn(async move {
        let reply = |resp: ahand_protocol::FileResponse| {
            let env = Envelope {
                device_id: did.clone(),
                msg_id: new_msg_id(),
                ts_ms: now_ms(),
                payload: Some(envelope::Payload::FileResponse(resp)),
                ..Default::default()
            };
            let _ = tx_clone.send(env);
        };

        // Outcome of the wait: either we know whether the caller approved
        // or we bailed out because the connection closed / timed out.
        enum Outcome {
            Approved,
            Denied(String),
            TimedOut,
            ConnectionClosed,
        }

        let outcome = tokio::select! {
            result = tokio::time::timeout(timeout, approval_rx) => {
                match result {
                    Ok(Ok(resp)) if resp.approved => Outcome::Approved,
                    Ok(Ok(resp)) => {
                        let reason = if resp.reason.is_empty() {
                            "approval denied".to_string()
                        } else {
                            format!("approval denied: {}", resp.reason)
                        };
                        Outcome::Denied(reason)
                    }
                    _ => Outcome::TimedOut,
                }
            }
            changed = close_rx_clone.changed() => {
                // The watch channel flipped to true (or the sender dropped).
                // Either way the connection is going away — bail out.
                let _ = changed;
                Outcome::ConnectionClosed
            }
        };

        match outcome {
            Outcome::Approved => {
                info!(request_id = %response_request_id, "file approval granted");
                let response = file_mgr_clone.handle(&req).await;
                reply(response);
            }
            Outcome::Denied(reason) => {
                info!(request_id = %response_request_id, "file approval denied");
                amgr.expire(&approval_key).await;
                reply(crate::file_manager::error_response(
                    response_request_id.clone(),
                    ahand_protocol::FileErrorCode::PolicyDenied,
                    &req_paths_joined_for_task,
                    &reason,
                ));
            }
            Outcome::TimedOut => {
                info!(request_id = %response_request_id, "file approval timed out");
                amgr.expire(&approval_key).await;
                reply(crate::file_manager::error_response(
                    response_request_id.clone(),
                    ahand_protocol::FileErrorCode::PolicyDenied,
                    &req_paths_joined_for_task,
                    "approval timed out",
                ));
            }
            Outcome::ConnectionClosed => {
                // Connection is already torn down; no point replying via
                // `tx` — the receiver task has exited. Just clean up the
                // approval manager entry and let the task exit.
                info!(
                    request_id = %response_request_id,
                    "file approval cancelled: parent connection closed"
                );
                amgr.expire(&approval_key).await;
            }
        }
    });
}

// ── AppTool envelope helpers ──────────────────────────────────────────────────

fn app_tool_error_envelope(
    device_id: &str,
    tool_call_id: &str,
    code: &str,
    message: String,
) -> Envelope {
    Envelope {
        device_id: device_id.to_string(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::AppToolResponse(
            ahand_protocol::AppToolResponse {
                tool_call_id: tool_call_id.to_string(),
                result: Some(ahand_protocol::app_tool_response::Result::Error(
                    ahand_protocol::AppToolError {
                        code: code.to_string(),
                        message,
                    },
                )),
            },
        )),
        ..Default::default()
    }
}

fn app_tool_result_envelope(device_id: &str, tool_call_id: &str, result_json: String) -> Envelope {
    Envelope {
        device_id: device_id.to_string(),
        msg_id: new_msg_id(),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::AppToolResponse(
            ahand_protocol::AppToolResponse {
                tool_call_id: tool_call_id.to_string(),
                result: Some(ahand_protocol::app_tool_response::Result::ResultJson(
                    result_json,
                )),
            },
        )),
        ..Default::default()
    }
}

// ── AppTool handler ───────────────────────────────────────────────────────────

/// Handle an incoming AppToolRequest.
///
/// Revised step order (Task 6):
///   1. Idempotency check — early-exit if already Running (silent) or Completed
///      (replay cached response).
///   2. Lookup — TOOL_NOT_FOUND if not registered. Lookup must happen before the
///      gate so TOOL_NOT_FOUND is always returned for unknown tools regardless of
///      session mode (no information leak about mode from the error code).
///   3. Session-mode gate — uses a synthetic JobRequest keyed on a namespaced
///      `"app-tool:{tool_call_id}"` job_id (see approval_job_id below) so the
///      existing approval machinery (ApprovalManager, spawned-wait pattern)
///      is reused without modification.
///      * Inactive / expired trust → SESSION_INACTIVE error, return.
///      * Allow (Trust/AutoAccept) + requires_approval=false → fall through.
///      * Allow + requires_approval=true OR Strict → NeedsApproval path:
///        mark_running first (idempotency), send ApprovalRequest, spawn waiter.
///   4. Parse args — INVALID_ARGS on bad JSON or non-object.
///   5. Concurrency permit — CONCURRENCY_LIMIT if all slots taken.
///   6. Execute (spawned).
///
/// mark_running is called BEFORE sending the ApprovalRequest (NeedsApproval path)
/// or before spawning execution (Allow path). This closes the idempotency window:
/// a duplicate request arriving while approval is pending or while execution is
/// running sees Running and is silently ignored. Every terminal outcome
/// (denied / timed out / handler result) calls mark_completed so that a
/// post-completion replay of the same tool_call_id returns the cached result.
///
/// Note: the handler captured at step 2 is the one registered at request time.
/// If the app re-registers the same tool mid-wait (between step 3 NeedsApproval
/// and waiter resolution), the captured handler still executes — we approve
/// what was requested, not what happens to be registered at approval time.
///
/// Contrast with handle_file_request (which also spawns its approval-wait leg):
/// file requests front-load a quick policy check inline and only spawn the
/// approval waiter; here the entire execution is spawned because handler
/// duration is unbounded (up to MAX_TIMEOUT_MS = 300s).
///
/// Timeout semantics: `timeout_ms` bounds the WHOLE invocation (approval +
/// execution). Approval wait = min(approval_timeout, clamp_timeout(timeout_ms));
/// execution gets the remaining budget (floored at MIN_TIMEOUT_MS). This ensures
/// the hub's `clamped_timeout + 2s` window always covers the daemon-side outcome
/// and eliminates ghost execution (where a late approval would run a handler
/// whose result nobody can receive because the hub has already moved on).
///
/// The approval waiter intentionally does NOT watch `close_rx` (unlike file-request
/// waiters): the response rides the stamped outbox so a result produced across a
/// reconnect is replayed to the hub; the cost is an orphaned waiter for at most
/// `min(approval_timeout, 300 s)` for app tools — not watching `close_rx` is
/// therefore cheap, because the waiter self-cancels within the invocation deadline.
#[allow(clippy::too_many_arguments)]
async fn handle_app_tool_request<T>(
    device_id: &str,
    caller_uid: &str,
    req: ahand_protocol::AppToolRequest,
    tx: &T,
    session_mgr: &Arc<SessionManager>,
    approval_mgr: &Arc<ApprovalManager>,
    approval_broadcast_tx: &broadcast::Sender<Envelope>,
    app_tools: &Arc<AppToolRegistry>,
) where
    T: crate::executor::EnvelopeSink,
{
    info!(
        tool_call_id = %req.tool_call_id,
        name = %req.name,
        "received AppToolRequest"
    );

    // ── Step 1: idempotency check ─────────────────────────────────────────
    match app_tools.call_state(&req.tool_call_id).await {
        crate::app_tool_registry::CallState::Running => {
            warn!(
                tool_call_id = %req.tool_call_id,
                "duplicate tool_call_id while running — ignoring"
            );
            return;
        }
        crate::app_tool_registry::CallState::Completed(cached) => {
            debug!(
                tool_call_id = %req.tool_call_id,
                "duplicate tool_call_id after completion — replaying cached response"
            );
            // Replay the cached response (error or result).
            let envelope = match (cached.result_json, cached.error) {
                (Some(json), _) => app_tool_result_envelope(device_id, &req.tool_call_id, json),
                (None, Some(err)) => {
                    app_tool_error_envelope(device_id, &req.tool_call_id, &err.code, err.message)
                }
                (None, None) => {
                    // Shouldn't happen in practice but treat as HANDLER_ERROR.
                    app_tool_error_envelope(
                        device_id,
                        &req.tool_call_id,
                        "HANDLER_ERROR",
                        "cached result had no outcome".to_string(),
                    )
                }
            };
            let _ = tx.send(envelope);
            return;
        }
        crate::app_tool_registry::CallState::Unknown => {}
    }

    // ── Step 2: lookup ────────────────────────────────────────────────────
    // Lookup happens BEFORE the session gate so that TOOL_NOT_FOUND is always
    // returned for unknown tools regardless of session mode (no information
    // leak about mode). The gate needs descriptor.requires_approval, so we
    // look up once and reuse both descriptor and handler.
    let (descriptor, handler) = match app_tools.lookup(&req.name).await {
        Some(pair) => pair,
        None => {
            warn!(
                tool_call_id = %req.tool_call_id,
                name = %req.name,
                "app tool not found"
            );
            let _ = tx.send(app_tool_error_envelope(
                device_id,
                &req.tool_call_id,
                "TOOL_NOT_FOUND",
                format!(
                    "no app tool named {:?} is registered on this device",
                    req.name
                ),
            ));
            return;
        }
    };

    // ── Step 3: session-mode gate ─────────────────────────────────────────
    // Build a synthetic JobRequest so the existing SessionManager and
    // ApprovalManager work unchanged. The job_id is namespaced "app-tool:"
    // so a cloud-chosen tool_call_id can never accidentally evict a real
    // job's pending approval in ApprovalManager's shared HashMap — mirroring
    // the "file-req:" namespace used by handle_file_request (~line 1615).
    // ApprovalResponse echoes ApprovalRequest.job_id, so resolve() still
    // works: we send "app-tool:{tool_call_id}" and receive it back unchanged.
    let approval_job_id = format!("app-tool:{}", req.tool_call_id);
    let args_preview: String = req.args_json.chars().take(512).collect();
    let synthetic = ahand_protocol::JobRequest {
        job_id: approval_job_id.clone(),
        tool: format!("app:{}", req.name),
        args: vec![args_preview],
        ..Default::default()
    };

    let decision = match session_mgr.check(&synthetic, caller_uid).await {
        SessionDecision::Deny(reason) => {
            warn!(
                tool_call_id = %req.tool_call_id,
                name = %req.name,
                reason = %reason,
                "app tool rejected by session mode"
            );
            let _ = tx.send(app_tool_error_envelope(
                device_id,
                &req.tool_call_id,
                "SESSION_INACTIVE",
                format!("session mode rejects app tool calls: {reason}"),
            ));
            return;
        }
        // Allow but requires_approval=true: upgrade to NeedsApproval.
        SessionDecision::Allow if descriptor.requires_approval => {
            let previous_refusals = session_mgr.get_refusals(&synthetic.tool).await;
            SessionDecision::NeedsApproval {
                reason: format!(
                    "app tool {:?} is registered with requires_approval",
                    req.name
                ),
                previous_refusals,
            }
        }
        other => other,
    };

    if let SessionDecision::NeedsApproval {
        reason,
        previous_refusals,
    } = decision
    {
        info!(
            tool_call_id = %req.tool_call_id,
            name = %req.name,
            reason = %reason,
            "app tool requires approval"
        );

        // Compute the bounded approval-wait timeout BEFORE submitting so the
        // advertised expires_ms in the ApprovalRequest matches the window the
        // waiter actually uses (dial + dialog UI both show this value).
        let timeout_ms_req = req.timeout_ms;
        // The call's clamped timeout is the TOTAL deadline for approval +
        // execution. Bounding the approval wait by it guarantees the hub's
        // `clamped + 2s` wait always covers the daemon-side outcome — no ghost
        // execution after the caller's window closed (the hub mints a fresh
        // tool_call_id per POST, so a late result would be unreachable anyway).
        // The operator's approval_timeout remains an upper bound — a caller's
        // timeout_ms can shrink the window but never extend it beyond what the
        // approval policy permits.
        let total_deadline_ms = AppToolRegistry::clamp_timeout(timeout_ms_req);
        let timeout = approval_mgr
            .default_timeout()
            .min(Duration::from_millis(total_deadline_ms as u64));

        // Mark running BEFORE sending the ApprovalRequest so that a
        // duplicate request arriving while approval is pending sees Running
        // and is silently ignored (idempotency window is closed now).
        // Every terminal outcome (deny/timeout/execute) calls mark_completed.
        app_tools.mark_running(&req.tool_call_id).await;

        let (approval_req, approval_rx) = approval_mgr
            .submit_with_timeout(
                synthetic.clone(),
                caller_uid,
                reason,
                previous_refusals,
                timeout,
            )
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

        // Spawn a task to wait for approval without blocking the WS read loop.
        let tx_clone = (*tx).clone();
        let did = device_id.to_string();
        let amgr = Arc::clone(approval_mgr);
        let app_tools_clone = Arc::clone(app_tools);
        let tool_call_id = req.tool_call_id.clone();
        let tool_name = req.name.clone();
        let args_json = req.args_json.clone();
        let context_json = req.context_json.clone();
        let approval_job_id_clone = approval_job_id.clone();

        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let result = tokio::time::timeout(timeout, approval_rx).await;
            match result {
                Ok(Ok(resp)) if resp.approved => {
                    let approval_wait_ms = started.elapsed().as_millis() as u64;
                    // Execution gets what's left of the total deadline
                    // (clamp_timeout inside validate_and_execute floors this
                    // at MIN_TIMEOUT_MS).
                    let remaining_ms = (total_deadline_ms as u64)
                        .saturating_sub(approval_wait_ms)
                        .max(crate::app_tool_registry::MIN_TIMEOUT_MS as u64)
                        as u32;
                    info!(
                        tool_call_id = %tool_call_id,
                        tool_name = %tool_name,
                        approval_wait_ms,
                        remaining_ms,
                        "app tool approval granted"
                    );
                    // Proceed to args parse → permit → execute inside this task.
                    validate_and_execute_app_tool(
                        &did,
                        tool_call_id,
                        tool_name,
                        args_json,
                        context_json,
                        remaining_ms,
                        handler,
                        &tx_clone,
                        &app_tools_clone,
                    )
                    .await;
                }
                Ok(Ok(resp)) => {
                    // Denied.
                    // refusal is recorded at the ApprovalResponse resolve site
                    // (see ~line 650); recording here would duplicate it.
                    let approval_wait_ms = started.elapsed().as_millis() as u64;
                    info!(
                        tool_call_id = %tool_call_id,
                        tool_name = %tool_name,
                        approval_wait_ms,
                        reason = %resp.reason,
                        "app tool approval denied"
                    );
                    // resolve() already removed the entry from ApprovalManager;
                    // calling expire here would be a no-op — skip it.
                    let denied_reason = if resp.reason.is_empty() {
                        "the user declined this call".to_string()
                    } else {
                        format!("the user declined this call: {}", resp.reason)
                    };
                    fail_app_tool_call(
                        &app_tools_clone,
                        &tx_clone,
                        &did,
                        &tool_call_id,
                        "APPROVAL_DENIED",
                        denied_reason,
                    )
                    .await;
                }
                _ => {
                    // Timeout (or channel closed).
                    let approval_wait_ms = started.elapsed().as_millis() as u64;
                    warn!(
                        tool_call_id = %tool_call_id,
                        tool_name = %tool_name,
                        approval_wait_ms,
                        "app tool approval timed out"
                    );
                    // expire() is needed here: resolve() was never called (no
                    // response arrived), so the entry is still in the HashMap.
                    amgr.expire(&approval_job_id_clone).await;
                    fail_app_tool_call(
                        &app_tools_clone,
                        &tx_clone,
                        &did,
                        &tool_call_id,
                        "APPROVAL_TIMEOUT",
                        "approval request expired without a user response".to_string(),
                    )
                    .await;
                }
            }
        });

        return; // spawned task owns the rest; read loop continues
    }

    // ── Allow path: fall through to args parse → permit → execute ────────
    validate_and_execute_app_tool(
        device_id,
        req.tool_call_id,
        req.name,
        req.args_json,
        req.context_json,
        req.timeout_ms,
        handler,
        tx,
        app_tools,
    )
    .await;
}

/// Record a failed app-tool call in the idempotency cache and send the error
/// envelope to the WS. Used at all app-tool fail paths (deny, timeout,
/// bad-JSON/non-object args, invalid context, concurrency limit) to keep them
/// DRY and avoid the double-construction drift risk where code and message
/// diverge between mark_completed and the envelope.
async fn fail_app_tool_call<T: crate::executor::EnvelopeSink>(
    app_tools: &Arc<AppToolRegistry>,
    tx: &T,
    device_id: &str,
    tool_call_id: &str,
    code: &str,
    message: String,
) {
    app_tools
        .mark_completed(
            tool_call_id.to_string(),
            crate::app_tool_registry::CompletedAppToolCall {
                result_json: None,
                error: Some(crate::app_tool_registry::AppToolError {
                    code: code.to_string(),
                    message: message.clone(),
                }),
            },
        )
        .await;
    let _ = tx.send(app_tool_error_envelope(
        device_id,
        tool_call_id,
        code,
        message,
    ));
}

/// Post-gate tail: parse args, acquire permit, mark_running, spawn execute.
///
/// Called from two sites:
///   1. Inline Allow path in handle_app_tool_request.
///   2. Inside the approval-wait task after approval is granted.
///
/// The caller must NOT have called mark_running yet on the Allow path (it is
/// called here). On the approval path, mark_running was called before the
/// ApprovalRequest was sent (to close the idempotency window during the wait),
/// so this function's mark_running call is a no-op duplicate — the registry
/// treats a second mark_running as idempotent (still Running).
#[allow(clippy::too_many_arguments)]
async fn validate_and_execute_app_tool<T>(
    device_id: &str,
    tool_call_id: String,
    tool_name: String,
    args_json: String,
    context_json: String,
    timeout_ms_req: u32,
    handler: crate::app_tool_registry::AppToolHandler,
    tx: &T,
    app_tools: &Arc<AppToolRegistry>,
) where
    T: crate::executor::EnvelopeSink,
{
    // ── Parse args ────────────────────────────────────────────────────────
    let args: serde_json::Value = match serde_json::from_str(&args_json) {
        Ok(v) => v,
        Err(err) => {
            warn!(
                tool_call_id = %tool_call_id,
                tool_name = %tool_name,
                error = %err,
                "invalid app tool args"
            );
            // On the approval path mark_running was already called; record
            // completion so the idempotency cache is consistent.
            fail_app_tool_call(
                app_tools,
                tx,
                device_id,
                &tool_call_id,
                "INVALID_ARGS",
                format!("args_json is not valid JSON: {err}"),
            )
            .await;
            return;
        }
    };
    if !args.is_object() {
        warn!(
            tool_call_id = %tool_call_id,
            tool_name = %tool_name,
            "invalid app tool args: not a JSON object"
        );
        fail_app_tool_call(
            app_tools,
            tx,
            device_id,
            &tool_call_id,
            "INVALID_ARGS",
            "args_json must be a JSON object".to_string(),
        )
        .await;
        return;
    }

    let context = if context_json.is_empty() {
        None
    } else {
        match serde_json::from_str::<serde_json::Value>(&context_json) {
            Ok(v) if v.is_object() => Some(v),
            Ok(_) => {
                warn!(
                    tool_call_id = %tool_call_id,
                    tool_name = %tool_name,
                    "invalid app tool context: not a JSON object"
                );
                fail_app_tool_call(
                    app_tools,
                    tx,
                    device_id,
                    &tool_call_id,
                    "INVALID_ARGS",
                    "context_json must be a JSON object".to_string(),
                )
                .await;
                return;
            }
            Err(err) => {
                warn!(
                    tool_call_id = %tool_call_id,
                    tool_name = %tool_name,
                    error = %err,
                    "invalid app tool context"
                );
                fail_app_tool_call(
                    app_tools,
                    tx,
                    device_id,
                    &tool_call_id,
                    "INVALID_ARGS",
                    format!("context_json is not valid JSON: {err}"),
                )
                .await;
                return;
            }
        }
    };

    // ── Concurrency permit (fail-fast) ────────────────────────────────────
    let permit = match app_tools.try_acquire_permit() {
        Some(p) => p,
        None => {
            warn!(
                tool_call_id = %tool_call_id,
                tool_name = %tool_name,
                "app tool concurrency limit hit"
            );
            fail_app_tool_call(
                app_tools,
                tx,
                device_id,
                &tool_call_id,
                "CONCURRENCY_LIMIT",
                "too many app tool calls in flight on this device; retry after in-flight calls finish".to_string(),
            )
            .await;
            return;
        }
    };

    // ── Execute (spawned so the read loop / approval task stays unblocked) ─
    // mark_running BEFORE spawning so any duplicate that arrives before the
    // task starts still sees Running (idempotency window is closed here).
    // On the approval path this is already Running — mark_running is
    // idempotent; calling it again is safe.
    app_tools.mark_running(&tool_call_id).await;

    let tx_clone = (*tx).clone();
    let did = device_id.to_string();
    let app_tools_clone = Arc::clone(app_tools);
    let timeout_ms = AppToolRegistry::clamp_timeout(timeout_ms_req);
    let invocation = AppToolInvocation { args, context };

    tokio::spawn(async move {
        execute_app_tool(
            &did,
            tool_call_id,
            tool_name,
            invocation,
            handler,
            permit,
            timeout_ms,
            &tx_clone,
            &app_tools_clone,
        )
        .await;
    });
}

/// Execute an app tool handler with panic isolation, timeout, and idempotency
/// recording. Called from a spawned task (not the read loop).
///
/// The permit is moved into the spawned handler task (not held by this fn).
/// If timeout fires before the handler completes, we do NOT abort the spawned
/// handler task — it may hold app-owned locks or resources. The permit releases
/// naturally when the handler task eventually finishes (or drops).
#[allow(clippy::too_many_arguments)]
async fn execute_app_tool<T>(
    device_id: &str,
    tool_call_id: String,
    tool_name: String,
    invocation: AppToolInvocation,
    handler: crate::app_tool_registry::AppToolHandler,
    permit: tokio::sync::OwnedSemaphorePermit,
    timeout_ms: u32,
    tx: &T,
    app_tools: &Arc<AppToolRegistry>,
) where
    T: crate::executor::EnvelopeSink,
{
    let started = std::time::Instant::now();
    let timeout = std::time::Duration::from_millis(timeout_ms as u64);

    // Spawn the handler inside its own task for panic isolation.
    // The permit is moved into the task so it is released when the task
    // finishes (or is eventually garbage-collected after a timeout — we do NOT
    // abort() the task because the handler may hold app-owned locks).
    let handler_task = tokio::spawn(async move {
        let _permit = permit; // keep permit alive until handler completes
        handler(invocation).await
    });

    let (code, message, result_json) = match tokio::time::timeout(timeout, handler_task).await {
        // Handler completed successfully.
        Ok(Ok(Ok(value))) => {
            let duration_ms = started.elapsed().as_millis() as u64;
            info!(
                tool_call_id = %tool_call_id,
                tool_name = %tool_name,
                duration_ms,
                outcome = "success",
                "app tool call completed"
            );
            (None, None, Some(value.to_string()))
        }
        // Handler returned an error.
        Ok(Ok(Err(app_err))) => {
            let code = if app_err.code.is_empty() {
                "HANDLER_ERROR".to_string()
            } else {
                app_err.code.clone()
            };
            let duration_ms = started.elapsed().as_millis() as u64;
            info!(
                tool_call_id = %tool_call_id,
                tool_name = %tool_name,
                duration_ms,
                outcome = %code,
                "app tool call returned handler error"
            );
            (Some(code), Some(app_err.message), None)
        }
        // Handler task panicked.
        Ok(Err(join_err)) if join_err.is_panic() => {
            let duration_ms = started.elapsed().as_millis() as u64;
            warn!(
                tool_call_id = %tool_call_id,
                tool_name = %tool_name,
                duration_ms,
                "app tool handler panicked; the app may be in a bad state"
            );
            (
                Some("HANDLER_PANIC".to_string()),
                Some("app tool handler panicked; the app may be in a bad state".to_string()),
                None,
            )
        }
        // Handler task was cancelled (should not happen with our spawn pattern).
        Ok(Err(_)) => {
            let duration_ms = started.elapsed().as_millis() as u64;
            warn!(
                tool_call_id = %tool_call_id,
                tool_name = %tool_name,
                duration_ms,
                "app tool task was cancelled"
            );
            (
                Some("HANDLER_ERROR".to_string()),
                Some("app tool task was cancelled".to_string()),
                None,
            )
        }
        // Timeout — handler task is still running but we stop waiting.
        // We do NOT abort the spawned task: it may hold app-owned locks.
        // The permit releases when the handler task eventually finishes.
        Err(_) => {
            let duration_ms = started.elapsed().as_millis() as u64;
            warn!(
                tool_call_id = %tool_call_id,
                tool_name = %tool_name,
                duration_ms,
                timeout_ms,
                "app tool timed out"
            );
            (
                Some("EXECUTION_TIMEOUT".to_string()),
                Some(format!("app tool did not finish within {timeout_ms}ms")),
                None,
            )
        }
    };

    // Record outcome in the idempotency cache (required on every path).
    let completed = crate::app_tool_registry::CompletedAppToolCall {
        result_json: result_json.clone(),
        error: match (&code, &message) {
            (Some(c), Some(m)) => Some(crate::app_tool_registry::AppToolError {
                code: c.clone(),
                message: m.clone(),
            }),
            _ => None,
        },
    };
    app_tools
        .mark_completed(tool_call_id.clone(), completed)
        .await;

    // Send wire response.
    let envelope = match (result_json, code, message) {
        (Some(json), _, _) => app_tool_result_envelope(device_id, &tool_call_id, json),
        (None, Some(c), Some(m)) => app_tool_error_envelope(device_id, &tool_call_id, &c, m),
        _ => app_tool_error_envelope(
            device_id,
            &tool_call_id,
            "HANDLER_ERROR",
            "internal: no outcome recorded".to_string(),
        ),
    };
    let _ = tx.send(envelope);
}

fn file_op_name(op: &ahand_protocol::file_request::Operation) -> &'static str {
    use ahand_protocol::file_request::Operation;
    match op {
        Operation::ReadText(_) => "read_text",
        Operation::ReadBinary(_) => "read_binary",
        Operation::ReadImage(_) => "read_image",
        Operation::ReadPdf(_) => "read_pdf",
        Operation::Write(_) => "write",
        Operation::Edit(_) => "edit",
        Operation::Delete(_) => "delete",
        Operation::Chmod(_) => "chmod",
        Operation::Stat(_) => "stat",
        Operation::List(_) => "list",
        Operation::Glob(_) => "glob",
        Operation::Mkdir(_) => "mkdir",
        Operation::Copy(_) => "copy",
        Operation::Move(_) => "move",
        Operation::CreateSymlink(_) => "create_symlink",
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

async fn recv_hello_challenge<S>(
    stream: &mut futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<S>>,
) -> Result<HelloChallenge, ConnectError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let Some(message) = stream.next().await else {
        return Err(ConnectError::Session(anyhow::anyhow!(
            "websocket closed before hello challenge"
        )));
    };
    let message = message
        .map_err(anyhow::Error::from)
        .map_err(ConnectError::Session)?;
    let tungstenite::Message::Binary(data) = message else {
        return Err(ConnectError::Session(anyhow::anyhow!(
            "expected binary hello challenge frame"
        )));
    };
    let envelope = Envelope::decode(data.as_ref())
        .map_err(anyhow::Error::from)
        .map_err(ConnectError::Session)?;
    match envelope.payload {
        Some(envelope::Payload::HelloChallenge(challenge)) => Ok(challenge),
        _ => Err(ConnectError::Session(anyhow::anyhow!(
            "expected hello challenge envelope"
        ))),
    }
}

async fn recv_hello_accepted<S>(
    stream: &mut futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<S>>,
) -> Result<HelloAccepted, ConnectError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let Some(message) = stream.next().await else {
        return Err(ConnectError::Session(anyhow::anyhow!(
            "websocket closed before hello accepted"
        )));
    };
    let message = message
        .map_err(anyhow::Error::from)
        .map_err(ConnectError::Session)?;
    classify_hello_accepted_message(message)
}

fn classify_hello_accepted_message(
    message: tungstenite::Message,
) -> Result<HelloAccepted, ConnectError> {
    let tungstenite::Message::Binary(data) = message else {
        return match message {
            tungstenite::Message::Close(Some(frame))
                if frame.code == tungstenite::protocol::frame::coding::CloseCode::Policy
                    && frame.reason == "auth-rejected" =>
            {
                Err(ConnectError::HandshakeRejected(anyhow::anyhow!(
                    "hello auth rejected"
                )))
            }
            tungstenite::Message::Close(Some(frame)) => {
                Err(ConnectError::Session(anyhow::anyhow!(
                    "websocket closed before hello accepted: code={} reason={}",
                    frame.code,
                    frame.reason
                )))
            }
            tungstenite::Message::Close(None) => Err(ConnectError::Session(anyhow::anyhow!(
                "websocket closed before hello accepted"
            ))),
            _ => Err(ConnectError::Session(anyhow::anyhow!(
                "expected binary hello accepted frame"
            ))),
        };
    };
    let envelope = Envelope::decode(data.as_ref())
        .map_err(anyhow::Error::from)
        .map_err(ConnectError::Session)?;
    match envelope.payload {
        Some(envelope::Payload::HelloAccepted(accepted)) => Ok(accepted),
        _ => Err(ConnectError::Session(anyhow::anyhow!(
            "expected hello accepted envelope"
        ))),
    }
}

enum ConnectError {
    HandshakeRejected(anyhow::Error),
    Session(anyhow::Error),
}

impl From<anyhow::Error> for ConnectError {
    fn from(err: anyhow::Error) -> Self {
        Self::Session(err)
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::sync::{Arc, Mutex};

    use ahand_protocol::{Envelope, JobFinished, envelope};
    use tokio::sync::mpsc;
    use tokio_tungstenite::tungstenite::{
        Message,
        protocol::{CloseFrame, frame::coding::CloseCode},
    };

    use crate::executor::EnvelopeSink;
    use crate::outbox::Outbox;

    use super::{
        BufferedEnvelopeSender, ConnectError, OutboundFrame, classify_hello_accepted_message,
        connect_tcp_with_keepalive, hello_capabilities_from_wire_names,
    };

    #[test]
    fn hello_capabilities_preserve_router_order_and_names() {
        let capabilities = hello_capabilities_from_wire_names(vec![
            "exec".to_string(),
            "file".to_string(),
            "browser-playwright-cli".to_string(),
        ]);

        assert_eq!(capabilities, vec!["exec", "file", "browser-playwright-cli"]);
    }

    #[test]
    fn browser_unavailable_response_preserves_ids_and_install_hint() {
        let req = ahand_protocol::BrowserRequest {
            request_id: "browser-req-1".to_string(),
            session_id: "browser-session-1".to_string(),
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

        let resp = super::browser_unavailable_response(&req, &unavailable);

        assert_eq!(resp.request_id, "browser-req-1");
        assert_eq!(resp.session_id, "browser-session-1");
        assert!(!resp.success);
        assert!(resp.error.contains("browser capability unavailable"));
        assert!(
            resp.error.contains(
                "install plugin browser-playwright-cli through the host plugin installer"
            )
        );
    }

    #[test]
    fn file_unavailable_response_preserves_request_id_and_path() {
        let req = ahand_protocol::FileRequest {
            request_id: "file-req-1".to_string(),
            ..Default::default()
        };
        let unavailable = crate::plugin_runtime::CapabilityUnavailable {
            capability: crate::plugin_runtime::CapabilityKind::File,
            plugin_id: "file".to_string(),
            status: crate::plugin_runtime::PluginStatus::Blocked,
            reason: "host configuration disabled file operations".to_string(),
            remediation: crate::plugin_runtime::CapabilityRemediation::HostConfiguration {
                message: "enable file operations in host configuration".to_string(),
            },
        };

        let resp = super::file_unavailable_response(&req, "notes.txt", &unavailable);

        assert_eq!(resp.request_id, "file-req-1");
        match resp.result {
            Some(ahand_protocol::file_response::Result::Error(err)) => {
                assert_eq!(err.code, ahand_protocol::FileErrorCode::PolicyDenied as i32);
                assert_eq!(err.path, "notes.txt");
                assert!(
                    err.message
                        .contains("file capability unavailable: host configuration disabled")
                );
            }
            other => panic!("expected file error response, got {other:?}"),
        }
    }

    #[test]
    fn managed_runtime_interactive_rejection_preserves_job_id() {
        let req = ahand_protocol::JobRequest {
            job_id: "node-interactive-1".to_string(),
            tool: "plugin:node".to_string(),
            interactive: true,
            ..Default::default()
        };

        let env = super::managed_runtime_interactive_rejection("device-1", &req);

        assert_eq!(env.device_id, "device-1");
        match env.payload {
            Some(envelope::Payload::JobRejected(rejected)) => {
                assert_eq!(rejected.job_id, "node-interactive-1");
                assert!(rejected.reason.contains("plugin:node"));
                assert!(rejected.reason.contains("interactive"));
            }
            other => panic!("expected JobRejected, got {other:?}"),
        }
    }

    /// Spinning up a real `spawn(...)` daemon and reaching into its private
    /// TcpStream just to read getsockopt would mean exposing the socket
    /// across the public API for one test's benefit. Instead, drive the
    /// exact helper the daemon's connect path uses
    /// (`connect_tcp_with_keepalive`) against a throwaway local listener
    /// and read the keepalive options back via socket2 — same setter
    /// codepath, no API surface bloat.
    ///
    /// Without OS-level keepalive, the daemon would inherit macOS's 2-hour
    /// idle default and stay attached to a zombie socket for hours after
    /// laptop sleep/wake. This test pins the 30s/10s/3-retry budget that
    /// makes TCP keepalive a meaningful second line of defense behind the
    /// WS Ping/Pong watchdog.
    #[tokio::test]
    async fn socket_keepalive_options_are_set() {
        // Bind a TCP listener and spawn an accept-and-park task so the
        // dial completes; we never speak WebSocket on it.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let _accept = tokio::spawn(async move {
            let _ = listener.accept().await;
            // Hold the accepted socket open for the test's lifetime —
            // dropping it would race the assertion with a TCP close.
            std::future::pending::<()>().await;
        });

        let url = format!("ws://127.0.0.1:{port}/ws");
        let stream = connect_tcp_with_keepalive(&url).await.expect("dial ok");
        let sock = socket2::SockRef::from(&stream);

        assert!(
            sock.keepalive().expect("read SO_KEEPALIVE"),
            "SO_KEEPALIVE must be on after connect_tcp_with_keepalive",
        );

        // Linux/BSDs path: socket2 0.6 exposes typed getters with `tcp_` prefix.
        // Windows and macOS are excluded here (Windows: no typed getters;
        // macOS: uses raw getsockopt below).
        #[cfg(all(unix, not(any(target_os = "macos", target_os = "ios"))))]
        {
            assert_eq!(
                sock.tcp_keepalive_time().expect("read keepalive idle time"),
                std::time::Duration::from_secs(30),
                "TCP_KEEPIDLE must be 30s",
            );
            assert_eq!(
                sock.tcp_keepalive_interval()
                    .expect("read keepalive probe interval"),
                std::time::Duration::from_secs(10),
                "TCP_KEEPINTVL must be 10s",
            );
            assert_eq!(
                sock.tcp_keepalive_retries()
                    .expect("read keepalive retry count"),
                3,
                "TCP_KEEPCNT must be 3",
            );
        }

        // macOS path: socket2 0.5 doesn't expose typed getters for
        // TCP_KEEPALIVE / TCP_KEEPINTVL / TCP_KEEPCNT on Apple targets,
        // so read them via raw getsockopt — the same syscall surface the
        // production setter uses, just inverted.
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            assert_eq!(
                getsockopt_int(&stream, libc::TCP_KEEPALIVE),
                30,
                "TCP_KEEPALIVE (idle seconds) must be 30",
            );
            assert_eq!(
                getsockopt_int(&stream, libc::TCP_KEEPINTVL),
                10,
                "TCP_KEEPINTVL must be 10s",
            );
            assert_eq!(
                getsockopt_int(&stream, libc::TCP_KEEPCNT),
                3,
                "TCP_KEEPCNT must be 3 on macOS",
            );
        }
    }

    /// Read a single `c_int` TCP-level socket option via raw getsockopt.
    /// Used by the macOS branch of `socket_keepalive_options_are_set`
    /// because socket2 0.5 doesn't expose typed getters for the keepalive
    /// timing options on Apple targets.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    fn getsockopt_int(stream: &tokio::net::TcpStream, opt: libc::c_int) -> libc::c_int {
        use std::os::fd::AsRawFd;
        let mut value: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let ret = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::IPPROTO_TCP,
                opt,
                &mut value as *mut libc::c_int as *mut libc::c_void,
                &mut len,
            )
        };
        assert_eq!(
            ret,
            0,
            "getsockopt({opt}) failed: {}",
            std::io::Error::last_os_error(),
        );
        value
    }

    #[test]
    fn auth_rejection_close_frame_is_classified_as_handshake_rejected() {
        let err = classify_hello_accepted_message(Message::Close(Some(CloseFrame {
            code: CloseCode::Policy,
            reason: Cow::Borrowed("auth-rejected"),
        })))
        .unwrap_err();

        assert!(matches!(err, ConnectError::HandshakeRejected(_)));
    }

    #[test]
    fn generic_close_frame_is_classified_as_session_error() {
        let err = classify_hello_accepted_message(Message::Close(None)).unwrap_err();

        assert!(matches!(err, ConnectError::Session(_)));
    }

    // ── Heartbeat task exit paths ──────────────────────────────────
    //
    // The task spawned by `spawn_heartbeat_task` must exit cleanly via
    // exactly two paths:
    //   1. The `EnvelopeSink::send` call returns `Err(())` because the
    //      underlying mpsc has been closed (e.g. session tore down).
    //   2. The returned `JoinHandle` is explicitly aborted.
    //
    // Both branches are exercised below so the coverage tooling doesn't
    // have to rely on the full integration test.

    #[derive(Clone)]
    struct CountingSink {
        count: Arc<std::sync::atomic::AtomicUsize>,
        fail_after: usize,
    }

    impl crate::executor::EnvelopeSink for CountingSink {
        fn send(&self, _envelope: Envelope) -> Result<(), ()> {
            let n = self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            if n > self.fail_after { Err(()) } else { Ok(()) }
        }
    }

    #[tokio::test]
    async fn heartbeat_task_exits_when_send_errors() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sink = CountingSink {
            count: count.clone(),
            fail_after: 1,
        };
        let handle = super::spawn_heartbeat_task(
            sink,
            "device-exit-send".into(),
            "test-version".into(),
            std::time::Duration::from_millis(10),
        );

        // Sink fails after the first successful send. Task should exit
        // within a few ticks.
        let joined = tokio::time::timeout(std::time::Duration::from_millis(500), handle)
            .await
            .expect("heartbeat task did not exit on send error");
        assert!(joined.is_ok(), "heartbeat task panicked: {joined:?}");
        assert!(
            count.load(std::sync::atomic::Ordering::SeqCst) >= 2,
            "expected at least one attempted send after the initial OK",
        );
    }

    #[tokio::test]
    async fn heartbeat_task_stops_when_aborted() {
        // Sink that never fails — exits only via abort.
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sink = CountingSink {
            count: count.clone(),
            fail_after: usize::MAX,
        };
        let handle = super::spawn_heartbeat_task(
            sink,
            "device-exit-abort".into(),
            "test-version".into(),
            std::time::Duration::from_millis(10),
        );
        // Let the ticker fire at least once so we exercise the tick arm.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        handle.abort();
        let joined = handle.await;
        // JoinHandle on aborted task resolves to JoinError::is_cancelled.
        assert!(
            joined
                .as_ref()
                .err()
                .map(|e| e.is_cancelled())
                .unwrap_or(false),
            "aborted heartbeat task should resolve as cancelled, got {joined:?}",
        );
    }

    #[test]
    fn buffered_envelope_sender_stores_frames_before_transport_send() {
        let outbox = Arc::new(Mutex::new(Outbox::new(16)));
        let (tx, rx) = mpsc::unbounded_channel::<OutboundFrame>();
        let sender = BufferedEnvelopeSender::new(tx, outbox.clone());

        sender
            .send(Envelope {
                device_id: "device-1".into(),
                payload: Some(envelope::Payload::JobFinished(JobFinished {
                    job_id: "job-1".into(),
                    exit_code: 0,
                    error: String::new(),
                })),
                ..Default::default()
            })
            .expect("send should enqueue");
        drop(rx);

        let buffered = outbox.lock().unwrap().drain_unacked();
        assert_eq!(buffered.len(), 1);
    }
}
