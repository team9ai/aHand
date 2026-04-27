use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio::sync::{Mutex as AsyncMutex, mpsc, watch};
use tokio::task::JoinHandle;

use ahand_hub_core::HubError;
use ahand_hub_core::traits::{DeviceStore, OutboxStore};
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::response::Response;

use crate::state::AppState;

const LEASE_RENEW_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);
const LOCK_ACQUIRE_RETRIES: u32 = 5;
const LOCK_ACQUIRE_BACKOFF: std::time::Duration = std::time::Duration::from_millis(200);

pub struct ConnectionRegistry {
    senders: DashMap<String, ConnectionEntry>,
    outbox: Arc<dyn OutboxStore>,
}

struct ConnectionEntry {
    active: Option<ActiveConnection>,
}

#[derive(Clone)]
struct ActiveConnection {
    connection_id: uuid::Uuid,
    /// UUID acting as the fencing token in every OutboxStore call.
    session_id: String,
    sender: mpsc::UnboundedSender<OutboundFrame>,
    close_tx: watch::Sender<bool>,
    /// Highest inbound seq observed from this device on this connection.
    /// Used both for dedup (`has_seen_inbound`) and to populate the `ack`
    /// field on outbound envelopes so the daemon can trim its own outbox.
    last_inbound_seq: Arc<AtomicU64>,
    /// Aborted on close so the lease renewer stops attempting to renew.
    lease_task: Arc<AsyncMutex<Option<JoinHandle<()>>>>,
    /// Aborted on close so the kick subscriber stops listening.
    kick_task: Arc<AsyncMutex<Option<JoinHandle<()>>>>,
}

pub struct OutboundFrame {
    pub frame: Vec<u8>,
}

impl OutboundFrame {
    /// Borrow the frame bytes as a slice. Convenience helper for
    /// integration tests that drain the receiver and want to decode
    /// without poking at the field directly.
    pub fn as_slice(&self) -> &[u8] {
        self.frame.as_slice()
    }
}

impl ConnectionRegistry {
    pub fn new(outbox: Arc<dyn OutboxStore>) -> Self {
        Self {
            senders: DashMap::new(),
            outbox,
        }
    }

    pub async fn register(
        &self,
        device_id: String,
        last_ack: u64,
    ) -> Result<
        (
            uuid::Uuid,
            mpsc::UnboundedReceiver<OutboundFrame>,
            watch::Receiver<bool>,
        ),
        HubError,
    > {
        let session_id = uuid::Uuid::new_v4().to_string();

        // 1) Acquire the lock with a kick-then-retry dance. The kick is
        //    addressed to the *previous* holder, but we publish ours as the
        //    "new owner" payload so subscribers can decide whether to bail.
        let mut acquired = self
            .outbox
            .try_acquire_lock(&device_id, &session_id)
            .await?;
        if !acquired {
            self.outbox.kick(&device_id, &session_id).await?;
            for _ in 0..LOCK_ACQUIRE_RETRIES {
                tokio::time::sleep(LOCK_ACQUIRE_BACKOFF).await;
                acquired = self
                    .outbox
                    .try_acquire_lock(&device_id, &session_id)
                    .await?;
                if acquired {
                    break;
                }
            }
        }
        if !acquired {
            return Err(HubError::OutboxLockContention(device_id));
        }

        // 2) Reconcile + read replay frames BEFORE we wire the in-process
        //    state, so a failure during reconcile leaves the lock in a
        //    clean state via the release_lock in the error arm.
        if let Err(err) = self
            .outbox
            .reconcile_on_hello(&device_id, &session_id, last_ack)
            .await
        {
            let _ = self.outbox.release_lock(&device_id, &session_id).await;
            return Err(err);
        }
        let replay = match self.outbox.unacked_frames(&device_id, last_ack).await {
            Ok(frames) => frames,
            Err(err) => {
                let _ = self.outbox.release_lock(&device_id, &session_id).await;
                return Err(err);
            }
        };

        // 3) Build per-connection state.
        let (tx, rx) = mpsc::unbounded_channel();
        let connection_id = uuid::Uuid::new_v4();
        let (close_tx, close_rx) = watch::channel(false);

        // 4) Spawn lease renewer.
        let lease_task = {
            let outbox = self.outbox.clone();
            let device_id = device_id.clone();
            let session_id = session_id.clone();
            let close_tx = close_tx.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(LEASE_RENEW_INTERVAL);
                interval.tick().await; // first tick is immediate; skip
                loop {
                    interval.tick().await;
                    match outbox.renew_lock(&device_id, &session_id).await {
                        Ok(true) => continue,
                        Ok(false) | Err(_) => {
                            tracing::warn!(
                                device_id = %device_id,
                                session_id = %session_id,
                                "lease lost, signalling close",
                            );
                            let _ = close_tx.send(true);
                            break;
                        }
                    }
                }
            })
        };

        // 5) Spawn kick subscriber.
        let kick_task = {
            let outbox = self.outbox.clone();
            let device_id = device_id.clone();
            let close_tx = close_tx.clone();
            tokio::spawn(async move {
                let mut sub = match outbox.subscribe_kick(&device_id).await {
                    Ok(sub) => sub,
                    Err(err) => {
                        tracing::warn!(
                            device_id = %device_id,
                            error = %err,
                            "failed to subscribe to kick channel",
                        );
                        return;
                    }
                };
                if sub.recv.changed().await.is_ok() {
                    tracing::info!(
                        device_id = %device_id,
                        "received kick, signalling close",
                    );
                    let _ = close_tx.send(true);
                }
            })
        };

        let active = ActiveConnection {
            connection_id,
            session_id: session_id.clone(),
            sender: tx.clone(),
            close_tx: close_tx.clone(),
            last_inbound_seq: Arc::new(AtomicU64::new(0)),
            lease_task: Arc::new(AsyncMutex::new(Some(lease_task))),
            kick_task: Arc::new(AsyncMutex::new(Some(kick_task))),
        };

        // 6) Push replay frames first, then publish the active connection.
        for frame in replay {
            let _ = tx.send(OutboundFrame { frame });
        }
        match self.senders.entry(device_id) {
            dashmap::mapref::entry::Entry::Occupied(mut entry) => {
                let entry = entry.get_mut();
                if let Some(prev) = entry.active.replace(active) {
                    let _ = prev.close_tx.send(true);
                }
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(ConnectionEntry {
                    active: Some(active),
                });
            }
        }
        Ok((connection_id, rx, close_rx))
    }

    pub(crate) async fn send(
        &self,
        device_id: &str,
        mut envelope: ahand_protocol::Envelope,
    ) -> anyhow::Result<()> {
        let (sender, session_id, connection_id, last_inbound_seq) = {
            let entry = self
                .senders
                .get(device_id)
                .ok_or_else(|| HubError::DeviceOffline(device_id.into()))?;
            let active = entry
                .active
                .as_ref()
                .ok_or_else(|| HubError::DeviceOffline(device_id.into()))?;
            (
                active.sender.clone(),
                active.session_id.clone(),
                active.connection_id,
                active.last_inbound_seq.clone(),
            )
        };

        let seq = match self.outbox.fenced_incr_seq(device_id, &session_id).await {
            Ok(seq) => seq,
            Err(HubError::Unauthorized) => {
                self.fail_connection(device_id, connection_id);
                return Err(HubError::DeviceOffline(device_id.into()).into());
            }
            Err(err) => return Err(err.into()),
        };
        envelope.seq = seq;
        envelope.ack = last_inbound_seq.load(Ordering::Relaxed);
        let frame = envelope.encode_to_vec();

        if let Err(err) = self
            .outbox
            .xadd_frame(device_id, &session_id, seq, frame.clone())
            .await
        {
            if matches!(err, HubError::Unauthorized) {
                self.fail_connection(device_id, connection_id);
                return Err(HubError::DeviceOffline(device_id.into()).into());
            }
            return Err(err.into());
        }

        if sender.send(OutboundFrame { frame }).is_err() {
            // The WS IO task is gone; the message stays in the durable
            // stream and will replay on next reconnect.
            self.fail_connection(device_id, connection_id);
            return Err(HubError::DeviceOffline(device_id.into()).into());
        }
        Ok(())
    }

    /// Mark the active connection for `device_id` dead if it still matches
    /// `connection_id`. Signals close_tx so the WS handler tears down. The
    /// background tasks are aborted by `unregister`, which is reached via
    /// the close_tx path.
    fn fail_connection(&self, device_id: &str, connection_id: uuid::Uuid) {
        if let Some(mut entry) = self.senders.get_mut(device_id)
            && entry
                .active
                .as_ref()
                .map(|a| a.connection_id == connection_id)
                .unwrap_or(false)
            && let Some(active) = entry.active.take()
        {
            let _ = active.close_tx.send(true);
        }
    }

    /// Forcibly close an active device WS. Returns true if there was an
    /// active connection to close, false otherwise. Idempotent: calling
    /// it on a device with no WS is a no-op. The main loop in
    /// `run_device_socket` picks up the `close_tx` signal, unregisters,
    /// and runs the normal teardown path.
    pub async fn kick_device(&self, device_id: &str) -> bool {
        let active = {
            let Some(entry) = self.senders.get(device_id) else {
                return false;
            };
            entry.active.clone()
        };
        let Some(active) = active else {
            return false;
        };
        let _ = active.close_tx.send(true);
        true
    }

    pub(crate) fn is_current(&self, device_id: &str, connection_id: uuid::Uuid) -> bool {
        self.senders
            .get(device_id)
            .map(|entry| {
                entry
                    .active
                    .as_ref()
                    .map(|active| active.connection_id == connection_id)
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    pub(crate) fn is_connected(&self, device_id: &str) -> bool {
        self.has_active(device_id)
    }

    /// Public wrapper around [`Self::is_connected`] so the
    /// control-plane HTTP handlers (which live outside the `ws`
    /// module) can gate job dispatch on a live WS without reaching
    /// into private state.
    pub fn is_online(&self, device_id: &str) -> bool {
        self.has_active(device_id)
    }

    /// Public wrapper around [`Self::send`] so the control-plane HTTP
    /// handlers can dispatch envelopes to a device.
    pub async fn send_envelope(
        &self,
        device_id: &str,
        envelope: ahand_protocol::Envelope,
    ) -> anyhow::Result<()> {
        self.send(device_id, envelope).await
    }

    /// True iff the device's connection has already observed an inbound
    /// envelope with seq >= `seq`. Used by the WS handler and JobRuntime
    /// to dedup retransmits — the durable stream is hub→device only, so
    /// this is per-connection state, not store-backed.
    pub(crate) fn has_seen_inbound(&self, device_id: &str, seq: u64) -> bool {
        if seq == 0 {
            return false;
        }
        self.senders
            .get(device_id)
            .and_then(|entry| entry.active.as_ref().map(|a| a.last_inbound_seq.clone()))
            .map(|last| last.load(Ordering::Relaxed) >= seq)
            .unwrap_or(false)
    }

    pub async fn observe_ack(&self, device_id: &str, ack: u64) -> anyhow::Result<()> {
        if ack == 0 {
            return Ok(());
        }
        // Fire-and-forget: the durable stream is the authority and the
        // next successful ack or MAXLEN trim will catch up if this one
        // fails. We log so the failure shows up in the hub's tracing.
        if let Err(err) = self.outbox.observe_ack(device_id, ack).await {
            tracing::warn!(
                device_id = %device_id,
                ack = ack,
                error = %err,
                "outbox observe_ack failed",
            );
        }
        Ok(())
    }

    pub(crate) async fn observe_inbound(
        &self,
        device_id: &str,
        seq: u64,
        ack: u64,
    ) -> anyhow::Result<()> {
        if seq > 0
            && let Some(entry) = self.senders.get(device_id)
            && let Some(active) = entry.active.as_ref()
        {
            active.last_inbound_seq.fetch_max(seq, Ordering::Relaxed);
        }
        self.observe_ack(device_id, ack).await
    }

    pub async fn unregister(
        &self,
        device_id: &str,
        connection_id: uuid::Uuid,
    ) -> anyhow::Result<bool> {
        // Take the active connection out of the map first (synchronous,
        // bounded by the DashMap shard lock) before we drop the guard
        // and switch to async work.
        let taken = {
            if let Some(mut entry) = self.senders.get_mut(device_id) {
                if entry
                    .active
                    .as_ref()
                    .map(|a| a.connection_id == connection_id)
                    .unwrap_or(false)
                {
                    entry.active.take()
                } else {
                    None
                }
            } else {
                None
            }
        };

        let Some(active) = taken else {
            return Ok(false);
        };

        let _ = active.close_tx.send(true);
        if let Some(handle) = active.lease_task.lock().await.take() {
            handle.abort();
        }
        if let Some(handle) = active.kick_task.lock().await.take() {
            handle.abort();
        }
        if let Err(err) = self
            .outbox
            .release_lock(device_id, &active.session_id)
            .await
        {
            tracing::warn!(
                device_id = %device_id,
                error = %err,
                "release_lock failed",
            );
        }
        // No active = nothing to keep around in the local map. The durable
        // stream lives on in the OutboxStore.
        self.senders.remove(device_id);
        Ok(true)
    }

    fn has_active(&self, device_id: &str) -> bool {
        self.senders
            .get(device_id)
            .map(|entry| entry.active.is_some())
            .unwrap_or(false)
    }
}

pub async fn handle_device_socket(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| async move {
        if let Err(err) = run_device_socket(socket, state).await {
            tracing::warn!(error = %err, "device socket ended with error");
        }
    })
}

async fn run_device_socket(socket: WebSocket, state: AppState) -> anyhow::Result<()> {
    let (mut sender, mut receiver) = socket.split();
    let mut active_connection: Option<(String, uuid::Uuid)> = None;
    let mut bootstrap_reservation: Option<crate::bootstrap::BootstrapReservation> = None;
    let mut send_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut presence_task: Option<tokio::task::JoinHandle<()>> = None;
    // Staleness monitor: closes the WS if no inbound frame (including the
    // daemon's Heartbeat envelopes) has arrived within
    // `device_staleness_timeout_ms`. Replaces the old hub-initiated ping
    // timer — direction is now daemon → hub.
    let mut staleness_monitor_task: Option<tokio::task::JoinHandle<()>> = None;
    let (local_close_tx, local_close_rx) = watch::channel(false);

    let run_result: anyhow::Result<()> = async {
        let challenge = issue_hello_challenge();
        sender
            .send(WsMessage::Binary(
                ahand_protocol::Envelope {
                    msg_id: "hello-challenge-0".into(),
                    ts_ms: challenge.issued_at_ms,
                    payload: Some(ahand_protocol::envelope::Payload::HelloChallenge(
                        challenge.clone(),
                    )),
                    ..Default::default()
                }
                .encode_to_vec()
                .into(),
            ))
            .await?;

        let Some(Ok(WsMessage::Binary(first_frame))) = receiver.next().await else {
            return Ok(());
        };

        let envelope = ahand_protocol::Envelope::decode(first_frame.as_ref())?;
        let hello = match envelope.payload {
            Some(ahand_protocol::envelope::Payload::Hello(hello)) => hello,
            _ => {
                let _ = send_handshake_close(
                    &mut sender,
                    axum::extract::ws::close_code::PROTOCOL,
                    "protocol-error",
                )
                .await;
                anyhow::bail!("expected hello envelope");
            }
        };

        let verified =
            match crate::auth::verify_device_hello(&envelope.device_id, &hello, &state, &challenge.nonce)
                .await
            {
                Ok(verified) => verified,
                Err(HubError::Unauthorized | HubError::InvalidSignature) => {
                    let _ = send_handshake_close(
                        &mut sender,
                        axum::extract::ws::close_code::POLICY,
                        "auth-rejected",
                    )
                    .await;
                    return Err(anyhow::Error::from(HubError::Unauthorized));
                }
                Err(err) => return Err(anyhow::Error::from(err)),
            };
        bootstrap_reservation = verified.bootstrap_reservation.clone();
        if let Err(err) = state
            .devices
            .accept_verified_hello(&envelope.device_id, &hello, &verified)
            .await
        {
            if matches!(err, HubError::Unauthorized | HubError::InvalidSignature) {
                let _ = send_handshake_close(
                    &mut sender,
                    axum::extract::ws::close_code::POLICY,
                    "auth-rejected",
                )
                .await;
            }
            return Err(anyhow::Error::from(err));
        }
        if let Some(reservation) = bootstrap_reservation.as_ref() {
            state.bootstrap_tokens.consume(reservation).await?;
            bootstrap_reservation = None;
        }
        if verified.allow_registration {
            state
                .append_audit_entry(
                    "device.registered",
                    "device",
                    &envelope.device_id,
                    &envelope.device_id,
                    serde_json::json!({
                        "hostname": hello.hostname,
                        "os": hello.os,
                        "capabilities": hello.capabilities,
                        "version": hello.version,
                        "auth_method": verified.auth_method,
                    }),
                )
                .await;
            // Best-effort webhook — the admin pre-register path sets
            // `external_user_id` before the daemon hellos, so we
            // look it up here. A failure to enqueue only surfaces as
            // a log warning; the handshake is already committed.
            let external_user_id = match state.devices.get(&envelope.device_id).await {
                Ok(Some(device)) => device.external_user_id,
                _ => None,
            };
            if let Err(err) = state
                .webhook
                .enqueue_registered(&envelope.device_id, external_user_id.as_deref())
                .await
            {
                tracing::warn!(
                    device_id = %envelope.device_id,
                    error = %err,
                    "failed to enqueue device.registered webhook",
                );
            }
        }
        let device_id = envelope.device_id.clone();
        let hostname = hello.hostname.clone();
        // Register BEFORE sending HelloAccepted so the in-process
        // DashMap entry is observable by the time the client sees the
        // accepted frame and starts dispatching jobs. With the
        // OutboxStore-backed registry, register involves Redis I/O —
        // sending HelloAccepted first would let a fast client race the
        // register completion and see is_online=false on its first
        // request.
        let (connection_id, mut outbound_rx, close_rx) = match state
            .connections
            .register(device_id.clone(), hello.last_ack)
            .await
        {
            Ok(tuple) => tuple,
            Err(HubError::OutboxLockContention(_)) => {
                let _ = send_handshake_close(
                    &mut sender,
                    axum::extract::ws::close_code::AGAIN,
                    "outbox-lock-contention",
                )
                .await;
                return Err(anyhow::Error::from(HubError::OutboxLockContention(
                    device_id.clone(),
                )));
            }
            Err(err) => return Err(anyhow::Error::from(err)),
        };
        sender
            .send(WsMessage::Binary(
                ahand_protocol::Envelope {
                    device_id: envelope.device_id.clone(),
                    msg_id: "hello-accepted-0".into(),
                    ts_ms: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64,
                    payload: Some(ahand_protocol::envelope::Payload::HelloAccepted(
                        ahand_protocol::HelloAccepted {
                            auth_method: verified.auth_method.into(),
                            update_suggestion: None,
                        },
                    )),
                    ..Default::default()
                }
                .encode_to_vec()
                .into(),
            ))
            .await?;
        let (control_tx, mut control_rx) = mpsc::unbounded_channel::<WsMessage>();
        let last_inbound_at = Arc::new(AsyncMutex::new(tokio::time::Instant::now()));
        active_connection = Some((device_id.clone(), connection_id));
        state.devices.mark_online(&device_id, "ws").await?;
        state.jobs.handle_device_connected(&device_id).await?;
        if let Err(err) = state.events.emit_device_online(&device_id, &hostname).await {
            tracing::warn!(device_id = %device_id, error = %err, "failed to write device.online audit");
        }
        // The external_user_id on the device row may have been set by an
        // earlier admin pre-register or during this hello's accept path.
        // Either way, fetch it fresh so webhooks always carry the most
        // recent attribution.
        let online_external_user_id = match state.devices.get(&device_id).await {
            Ok(Some(device)) => device.external_user_id,
            _ => None,
        };
        if let Err(err) = state
            .webhook
            .enqueue_online(&device_id, online_external_user_id.as_deref())
            .await
        {
            tracing::warn!(
                device_id = %device_id,
                error = %err,
                "failed to enqueue device.online webhook",
            );
        }

        if state.device_presence_refresh_ms > 0 {
            let devices = state.devices.clone();
            let refresh_ms = state.device_presence_refresh_ms;
            let refresh_device_id = device_id.clone();
            let mut refresh_close_rx = close_rx.clone();
            let mut refresh_local_close_rx = local_close_rx.clone();
            presence_task = Some(tokio::spawn(async move {
                loop {
                    tokio::select! {
                        biased;
                        _ = refresh_close_rx.changed() => break,
                        _ = refresh_local_close_rx.changed() => break,
                        _ = tokio::time::sleep(std::time::Duration::from_millis(refresh_ms)) => {
                            if let Err(err) = devices.mark_online(&refresh_device_id, "ws").await {
                                tracing::warn!(device_id = %refresh_device_id, error = %err, "failed to refresh device presence");
                                break;
                            }
                        }
                    }
                }
            }));
        }

        let connections = state.connections.clone();
        let send_device_id = device_id.clone();
        let mut send_close_rx = close_rx.clone();
        let mut send_local_close_rx = local_close_rx.clone();
        let send_local_close_tx = local_close_tx.clone();
        send_task = Some(tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = send_close_rx.changed() => break,
                    _ = send_local_close_rx.changed() => break,
                    control = control_rx.recv() => {
                        let Some(control) = control else {
                            break;
                        };
                        if sender.send(control).await.is_err() {
                            let _ = send_local_close_tx.send(true);
                            break;
                        }
                    }
                    outbound = outbound_rx.recv() => {
                        let Some(outbound) = outbound else {
                            break;
                        };
                        if !connections.is_current(&send_device_id, connection_id) {
                            break;
                        }
                        if sender
                            .send(WsMessage::Binary(outbound.frame.into()))
                            .await
                            .is_err()
                        {
                            let _ = send_local_close_tx.send(true);
                            break;
                        }
                    }
                }
            }
        }));

        if state.device_staleness_probe_interval_ms > 0 && state.device_staleness_timeout_ms > 0 {
            let probe_interval = state.device_staleness_probe_interval_ms;
            let staleness_timeout =
                std::time::Duration::from_millis(state.device_staleness_timeout_ms);
            let mut staleness_close_rx = close_rx.clone();
            let mut staleness_local_close_rx = local_close_rx.clone();
            let staleness_last_inbound_at = last_inbound_at.clone();
            let staleness_local_close_tx = local_close_tx.clone();
            staleness_monitor_task = Some(tokio::spawn(async move {
                // Periodically check how long it has been since the last
                // inbound frame. When the daemon's heartbeats stop arriving
                // (because the process or network died without closing the
                // WS cleanly), we trip the local close signal so the main
                // loop exits and the connection is reaped.
                //
                // No outbound probes are sent — direction is now daemon →
                // hub only.
                loop {
                    tokio::select! {
                        biased;
                        _ = staleness_close_rx.changed() => break,
                        _ = staleness_local_close_rx.changed() => break,
                        _ = tokio::time::sleep(std::time::Duration::from_millis(probe_interval)) => {
                            let elapsed = staleness_last_inbound_at.lock().await.elapsed();
                            if elapsed >= staleness_timeout {
                                let _ = staleness_local_close_tx.send(true);
                                break;
                            }
                        }
                    }
                }
            }));
        }

        let mut recv_close_rx = close_rx;
        let mut recv_local_close_rx = local_close_rx.clone();
        loop {
            let message = tokio::select! {
                biased;
                _ = recv_close_rx.changed() => break,
                _ = recv_local_close_rx.changed() => break,
                message = receiver.next() => {
                    let Some(message) = message else {
                        break;
                    };
                    message?
                }
            };
            if !state.connections.is_current(&device_id, connection_id) {
                break;
            }
            match message {
                WsMessage::Binary(frame) => {
                    *last_inbound_at.lock().await = tokio::time::Instant::now();
                    let envelope = ahand_protocol::Envelope::decode(frame.as_ref())?;
                    if state.connections.has_seen_inbound(&device_id, envelope.seq) {
                        state.connections.observe_ack(&device_id, envelope.ack).await?;
                    } else if let Some(ahand_protocol::envelope::Payload::Heartbeat(ref hb)) =
                        envelope.payload
                    {
                        // Heartbeat is observed (for ack bookkeeping and
                        // staleness refresh above) and then fanned out so
                        // downstream webhook senders / dashboards can mirror
                        // device presence without polling.
                        state
                            .connections
                            .observe_inbound(&device_id, envelope.seq, envelope.ack)
                            .await?;
                        let ttl = state
                            .device_expected_heartbeat_secs
                            .saturating_mul(3);
                        if let Err(err) = state
                            .events
                            .emit_device_heartbeat(&device_id, hb.sent_at_ms, ttl)
                            .await
                        {
                            tracing::warn!(
                                device_id = %device_id,
                                error = %err,
                                "failed to emit device.heartbeat event",
                            );
                        }
                        // Heartbeats are also posted to the external webhook
                        // gateway so team9's presence tracker can stay
                        // synced. Use the `online_external_user_id` cached
                        // at Hello-accept time instead of re-fetching the
                        // device row on every heartbeat — at fleet scale
                        // (~1000 devices @ 60s cadence) a per-heartbeat
                        // SELECT is ~17 Pg QPS of pure attribution
                        // lookups. The owner of a device effectively never
                        // changes mid-session, and if an admin revokes a
                        // device via DELETE /api/admin/devices/{id} the
                        // WS is torn down via `kick_device`, so staleness
                        // is bounded to the handshake-to-close window.
                        if state.webhook.is_enabled()
                            && let Err(err) = state
                                .webhook
                                .enqueue_heartbeat(
                                    &device_id,
                                    online_external_user_id.as_deref(),
                                    hb.sent_at_ms,
                                    ttl,
                                )
                                .await
                        {
                            tracing::warn!(
                                device_id = %device_id,
                                error = %err,
                                "failed to enqueue device.heartbeat webhook",
                            );
                        }
                    } else if let Some(ahand_protocol::envelope::Payload::BrowserResponse(ref browser_resp)) = envelope.payload {
                        if let Some((_, sender)) = state.browser_pending.remove(&browser_resp.request_id) {
                            let _ = sender.send(browser_resp.clone());
                        } else {
                            tracing::warn!(
                                request_id = %browser_resp.request_id,
                                "received BrowserResponse with no pending request"
                            );
                        }
                        state.connections.observe_inbound(&device_id, envelope.seq, envelope.ack).await?;
                    } else if dispatch_control_plane_event(&state, &envelope) {
                        // The envelope's job_id was registered in the
                        // control-plane tracker — the tee has already
                        // published the event to SSE subscribers.
                        // Skip JobRuntime (which would bail on an
                        // unknown job id) and just advance seq/ack.
                        state
                            .connections
                            .observe_inbound(&device_id, envelope.seq, envelope.ack)
                            .await?;
                    } else {
                        state.jobs.handle_device_frame(&device_id, &frame).await?;
                    }
                }
                WsMessage::Ping(payload) => {
                    *last_inbound_at.lock().await = tokio::time::Instant::now();
                    if control_tx.send(WsMessage::Pong(payload)).is_err() {
                        let _ = local_close_tx.send(true);
                        break;
                    }
                }
                WsMessage::Pong(_) => {
                    *last_inbound_at.lock().await = tokio::time::Instant::now();
                }
                WsMessage::Close(_) => break,
                _ => {}
            }
        }

        Ok(())
    }
    .await;

    if let Some(task) = send_task.take() {
        task.abort();
    }
    if let Some(task) = presence_task.take() {
        task.abort();
    }
    if let Some(task) = staleness_monitor_task.take() {
        task.abort();
    }
    if let Some(reservation) = bootstrap_reservation.take()
        && let Err(err) = state.bootstrap_tokens.release(&reservation).await
    {
        tracing::warn!(
            device_id = %reservation.device_id,
            error = %err,
            "failed to release bootstrap reservation"
        );
    }
    if let Some((device_id, connection_id)) = active_connection.take()
        && state
            .connections
            .unregister(&device_id, connection_id)
            .await?
    {
        let offline_external_user_id = match state.devices.get(&device_id).await {
            Ok(Some(device)) => device.external_user_id,
            _ => None,
        };
        state.devices.mark_offline(&device_id).await?;
        state.jobs.handle_device_disconnected(&device_id).await?;
        if let Err(err) = state.events.emit_device_offline(&device_id).await {
            tracing::warn!(device_id = %device_id, error = %err, "failed to write device.offline audit");
        }
        if let Err(err) = state
            .webhook
            .enqueue_offline(&device_id, offline_external_user_id.as_deref())
            .await
        {
            tracing::warn!(
                device_id = %device_id,
                error = %err,
                "failed to enqueue device.offline webhook",
            );
        }
    }

    run_result
}

/// Publish any job-related envelope received from a daemon to the
/// control-plane tracker, if the `job_id` is known to it.
///
/// Returns `true` if the envelope targeted a control-plane job (and
/// was therefore handled here) — the caller should then skip
/// [`crate::http::jobs::JobRuntime::handle_device_frame`] to avoid
/// the "job not found" bail-out that would kill the WS. Returns
/// `false` otherwise (not job-related, or a dashboard / JobRuntime
/// job id the caller still needs to process).
fn dispatch_control_plane_event(state: &AppState, envelope: &ahand_protocol::Envelope) -> bool {
    let Some(payload) = envelope.payload.as_ref() else {
        return false;
    };
    match payload {
        ahand_protocol::envelope::Payload::JobEvent(event) => {
            if state.control_jobs.get(&event.job_id).is_none() {
                return false;
            }
            let Some(kind) = event.event.as_ref() else {
                return true;
            };
            let control_event = match kind {
                ahand_protocol::job_event::Event::StdoutChunk(chunk) => {
                    crate::control_jobs::ControlJobEvent::Stdout {
                        chunk: String::from_utf8_lossy(chunk).into_owned(),
                    }
                }
                ahand_protocol::job_event::Event::StderrChunk(chunk) => {
                    crate::control_jobs::ControlJobEvent::Stderr {
                        chunk: String::from_utf8_lossy(chunk).into_owned(),
                    }
                }
                ahand_protocol::job_event::Event::Progress(percent) => {
                    crate::control_jobs::ControlJobEvent::Progress {
                        // Percent is a u32 on the wire but the SDK
                        // surface is 0..=100; clamp defensively.
                        percent: (*percent).min(100) as u8,
                        message: None,
                    }
                }
            };
            state.control_jobs.publish(&event.job_id, control_event);
            true
        }
        ahand_protocol::envelope::Payload::JobFinished(finished) => {
            let Some(channels) = state.control_jobs.get(&finished.job_id) else {
                return false;
            };
            let duration_ms = channels
                .started_at
                .elapsed()
                .as_millis()
                .min(u64::MAX as u128) as u64;
            // `error == "cancelled"` and non-zero exit_code are the
            // two ways a job can fail. We report them as an `error`
            // event so the SDK can distinguish graceful completion
            // from failure without peeking at the exit code.
            let event = if finished.exit_code == 0 && finished.error.is_empty() {
                crate::control_jobs::ControlJobEvent::Finished {
                    exit_code: finished.exit_code,
                    duration_ms,
                }
            } else {
                crate::control_jobs::ControlJobEvent::Error {
                    code: if finished.error == "cancelled" {
                        "cancelled".into()
                    } else {
                        "exec_failed".into()
                    },
                    message: if finished.error.is_empty() {
                        format!("exit code {}", finished.exit_code)
                    } else {
                        finished.error.clone()
                    },
                }
            };
            state.control_jobs.finalize(&finished.job_id, event);
            true
        }
        ahand_protocol::envelope::Payload::JobRejected(rejected) => {
            if state.control_jobs.get(&rejected.job_id).is_none() {
                return false;
            }
            state.control_jobs.finalize(
                &rejected.job_id,
                crate::control_jobs::ControlJobEvent::Error {
                    code: "rejected".into(),
                    message: rejected.reason.clone(),
                },
            );
            true
        }
        _ => false,
    }
}

fn issue_hello_challenge() -> ahand_protocol::HelloChallenge {
    ahand_protocol::HelloChallenge {
        nonce: uuid::Uuid::new_v4().into_bytes().to_vec(),
        issued_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
    }
}

async fn send_handshake_close(
    sender: &mut futures_util::stream::SplitSink<WebSocket, WsMessage>,
    code: u16,
    reason: &'static str,
) -> anyhow::Result<()> {
    sender
        .send(WsMessage::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await
        .map_err(anyhow::Error::from)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use prost::Message;

    use super::ConnectionRegistry;
    use crate::ws::in_memory_outbox::InMemoryOutboxStore;

    fn job_envelope(msg_id: &str, job_id: &str, arg: &str) -> ahand_protocol::Envelope {
        ahand_protocol::Envelope {
            device_id: "device-1".into(),
            msg_id: msg_id.into(),
            payload: Some(ahand_protocol::envelope::Payload::JobRequest(
                ahand_protocol::JobRequest {
                    job_id: job_id.into(),
                    tool: "echo".into(),
                    args: vec![arg.into()],
                    cwd: String::new(),
                    env: Default::default(),
                    timeout_ms: 30_000,
                    interactive: false,
                },
            )),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn register_replays_only_messages_after_last_ack() {
        let registry = ConnectionRegistry::new(Arc::new(InMemoryOutboxStore::new()));
        let (conn_a, mut rx_a, _close_a) = registry.register("device-1".into(), 0).await.unwrap();

        // Send 5 envelopes; drain each from the mpsc to confirm delivery.
        for i in 1..=5u8 {
            registry
                .send(
                    "device-1",
                    job_envelope(&format!("job-{i}"), &format!("job-{i}"), "x"),
                )
                .await
                .unwrap();
            let frame = rx_a.recv().await.expect("frame delivered");
            let env = ahand_protocol::Envelope::decode(frame.frame.as_slice()).unwrap();
            assert_eq!(env.seq, u64::from(i));
        }

        // Drop the live connection. unregister releases the lock so the
        // re-register below can grab it without going through the kick path.
        registry.unregister("device-1", conn_a).await.unwrap();

        // Re-register with last_ack=2 — only seqs 3..=5 should replay.
        let (_conn_b, mut rx_b, _close_b) = registry.register("device-1".into(), 2).await.unwrap();
        let mut replayed = Vec::new();
        while let Ok(Some(frame)) =
            tokio::time::timeout(Duration::from_millis(50), rx_b.recv()).await
        {
            replayed.push(frame);
        }
        assert_eq!(replayed.len(), 3, "only seqs 3..=5 should replay");
        let first = ahand_protocol::Envelope::decode(replayed[0].frame.as_slice()).unwrap();
        assert_eq!(first.seq, 3);
        let last = ahand_protocol::Envelope::decode(replayed[2].frame.as_slice()).unwrap();
        assert_eq!(last.seq, 5);
    }

    #[tokio::test]
    async fn inbound_seq_updates_outbound_ack() {
        let registry = ConnectionRegistry::new(Arc::new(InMemoryOutboxStore::new()));
        let (_connection_id, mut rx, _close_rx) =
            registry.register("device-1".into(), 0).await.unwrap();

        // Mirror the daemon→hub direction: an inbound envelope at seq=7
        // bumps the per-connection counter, so the next outbound envelope
        // must carry ack=7 in its header.
        registry.observe_inbound("device-1", 7, 0).await.unwrap();

        registry
            .send("device-1", job_envelope("job-1", "job-1", "hello"))
            .await
            .unwrap();

        let outbound = rx.recv().await.expect("frame delivered");
        let envelope = ahand_protocol::Envelope::decode(outbound.frame.as_slice()).unwrap();
        assert_eq!(envelope.seq, 1);
        assert_eq!(envelope.ack, 7);
    }

    #[tokio::test]
    async fn unregister_removes_idle_entries() {
        let registry = ConnectionRegistry::new(Arc::new(InMemoryOutboxStore::new()));
        let (connection_id, rx, _close_rx) = registry.register("device-1".into(), 0).await.unwrap();
        drop(rx);

        let removed = registry
            .unregister("device-1", connection_id)
            .await
            .unwrap();

        assert!(removed);
        assert!(registry.senders.is_empty());
    }

    #[tokio::test]
    async fn has_seen_inbound_uses_per_connection_counter() {
        let registry = ConnectionRegistry::new(Arc::new(InMemoryOutboxStore::new()));
        let (_connection_id, _rx, _close_rx) =
            registry.register("device-1".into(), 0).await.unwrap();

        assert!(!registry.has_seen_inbound("device-1", 5));
        registry.observe_inbound("device-1", 5, 0).await.unwrap();
        assert!(registry.has_seen_inbound("device-1", 5));
        // seq=0 is never seen (sentinel).
        assert!(!registry.has_seen_inbound("device-1", 0));
        // A lower seq is also "seen" (we track high water mark).
        assert!(registry.has_seen_inbound("device-1", 3));
        // A higher seq we haven't observed yet is not seen.
        assert!(!registry.has_seen_inbound("device-1", 6));
    }

    #[tokio::test]
    async fn second_register_returns_lock_contention_when_first_holds_lock() {
        // The lock held by the first session is the OutboxStore's, so the
        // second registration's kick-then-retry cycle exhausts and the
        // call surfaces `OutboxLockContention`. In a real Redis deployment
        // the kicked session would release on receiving the kick; the
        // in-memory impl doesn't model that, so this test is the
        // worst-case behavior. Once the holder unregisters, the next
        // register succeeds.
        let registry = ConnectionRegistry::new(Arc::new(InMemoryOutboxStore::new()));
        let (conn_a, _rx_a, _close_a) = registry.register("device-1".into(), 0).await.unwrap();

        let err = registry
            .register("device-1".into(), 0)
            .await
            .expect_err("should fail with contention");
        assert!(matches!(
            err,
            ahand_hub_core::HubError::OutboxLockContention(d) if d == "device-1",
        ));

        registry.unregister("device-1", conn_a).await.unwrap();
        // After the holder releases, a fresh register succeeds.
        let (_conn_b, _rx_b, _close_b) = registry.register("device-1".into(), 0).await.unwrap();
    }
}
