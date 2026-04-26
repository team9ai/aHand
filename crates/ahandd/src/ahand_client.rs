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
use tracing::{error, info, warn};

use crate::approval::ApprovalManager;
use crate::browser::BrowserManager;
use crate::config::Config;
use crate::device_identity::DeviceIdentity;
use crate::executor::{self, EnvelopeSink as _};
use crate::outbox::{Outbox, prepare_outbound};
use crate::registry::{IsKnown, JobRegistry};
use crate::session::{SessionDecision, SessionManager};
use crate::store::{Direction, RunStore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelloAuthMode {
    Ed25519,
    Bootstrap(String),
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
enum OutboundFrame {
    Envelope(QueuedEnvelope),
    WsPing(Vec<u8>),
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
    let (ws_stream, _) =
        tokio_tungstenite::client_async_tls_with_config(url, tcp, None, None)
            .await
            .map_err(anyhow::Error::from)
            .map_err(ConnectError::Session)?;
    let (mut sink, mut stream) = ws_stream.split();

    let challenge = recv_hello_challenge(&mut stream).await?;
    let last_ack = outbox.lock().expect("outbox mutex poisoned").local_ack();
    info!(last_ack, "connected, sending Hello");

    // Send Hello envelope — Hello is NOT stamped (seq=0), it's a connection signal.
    let hello = build_hello_envelope(
        device_id,
        identity,
        last_ack,
        browser_mgr.is_enabled(),
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

    if let Some(suggestion) = accepted.update_suggestion {
        info!(update_id = %suggestion.update_id, target = %suggestion.target_version,
            "hub suggests update during registration");
        let params = crate::updater::UpdateParams::from(suggestion);
        crate::updater::spawn_update(params, device_id.to_string(), tx.clone());
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
            Some(envelope::Payload::BrowserRequest(req)) => {
                handle_browser_request(device_id, caller_uid, &req, &tx, session_mgr, browser_mgr)
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

    // Abort the heartbeat task BEFORE dropping `tx`. The heartbeat task
    // holds its own clone of the `BufferedEnvelopeSender`, so if we
    // dropped `tx` first, the send task's mpsc receiver would still see
    // a live sender (from the heartbeat clone) and block on `rx.recv()`
    // indefinitely — `send_handle.await` would hang until the WS broke
    // naturally. Aborting the heartbeat task drops its sender clone, so
    // dropping `tx` here leaves zero senders and `rx.recv()` resolves to
    // `None`. Same logic for `ws_ping_task`.
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

pub fn build_hello_envelope(
    device_id: &str,
    identity: &DeviceIdentity,
    last_ack: u64,
    browser_enabled: bool,
    challenge_nonce: &[u8],
    bearer_token: Option<String>,
) -> Envelope {
    let signed_at_ms = identity.next_hello_signed_at_ms();
    let mut capabilities = vec!["exec".to_string()];
    if browser_enabled {
        capabilities.push("browser".to_string());
    }

    let mut hello = Hello {
        version: env!("CARGO_PKG_VERSION").to_string(),
        hostname: gethostname::gethostname().to_string_lossy().to_string(),
        os: std::env::consts::OS.to_string(),
        capabilities,
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

pub fn hello_auth_modes(bootstrap_token: Option<&str>) -> Vec<HelloAuthMode> {
    let mut modes = vec![HelloAuthMode::Ed25519];
    if let Some(token) = bootstrap_token {
        modes.push(HelloAuthMode::Bootstrap(token.to_owned()));
    }
    modes
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
) where
    T: crate::executor::EnvelopeSink,
{
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
async fn spawn_job<T>(
    device_id: &str,
    req: ahand_protocol::JobRequest,
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
            let (exit_code, error) = executor::run_job(did, req, tx_clone, cancel_rx, st).await;
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
) where
    T: crate::executor::EnvelopeSink,
{
    info!(
        request_id = %req.request_id,
        session_id = %req.session_id,
        action = %req.action,
        "received browser request"
    );

    if !browser_mgr.is_enabled() {
        let resp_env = Envelope {
            device_id: device_id.to_string(),
            msg_id: new_msg_id(),
            ts_ms: now_ms(),
            payload: Some(envelope::Payload::BrowserResponse(BrowserResponse {
                request_id: req.request_id.clone(),
                session_id: req.session_id.clone(),
                success: false,
                error: "browser capabilities not enabled".to_string(),
                ..Default::default()
            })),
            ..Default::default()
        };
        let _ = tx.send(resp_env);
        return;
    }

    // Session mode check using a synthetic JobRequest.
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
        BufferedEnvelopeSender, ConnectError, OutboundFrame, QueuedEnvelope,
        classify_hello_accepted_message, connect_tcp_with_keepalive,
    };

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

        // Linux/BSDs path: socket2 exposes typed getters.
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        {
            assert_eq!(
                sock.keepalive_time().expect("read keepalive idle time"),
                std::time::Duration::from_secs(30),
                "TCP_KEEPIDLE must be 30s",
            );
            assert_eq!(
                sock.keepalive_interval().expect("read keepalive probe interval"),
                std::time::Duration::from_secs(10),
                "TCP_KEEPINTVL must be 10s",
            );
            assert_eq!(
                sock.keepalive_retries().expect("read keepalive retry count"),
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
