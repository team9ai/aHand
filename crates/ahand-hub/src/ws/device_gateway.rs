use std::sync::{Arc, Mutex};

use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio::sync::{Mutex as AsyncMutex, mpsc, watch};

use ahand_hub_core::HubError;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::response::Response;

use crate::state::AppState;

#[derive(Default)]
pub struct ConnectionRegistry {
    senders: DashMap<String, ConnectionEntry>,
}

struct ConnectionEntry {
    active: Option<ActiveConnection>,
    outbox: Mutex<ahand_hub_core::Outbox>,
}

#[derive(Clone)]
struct ActiveConnection {
    connection_id: uuid::Uuid,
    sender: mpsc::UnboundedSender<OutboundFrame>,
    close_tx: watch::Sender<bool>,
}

pub(crate) struct OutboundFrame {
    pub(crate) frame: Vec<u8>,
}

impl ConnectionRegistry {
    pub(crate) fn register(
        &self,
        device_id: String,
        last_ack: u64,
    ) -> anyhow::Result<(
        uuid::Uuid,
        mpsc::UnboundedReceiver<OutboundFrame>,
        watch::Receiver<bool>,
    )> {
        let (tx, rx) = mpsc::unbounded_channel();
        let connection_id = uuid::Uuid::new_v4();
        let (close_tx, close_rx) = watch::channel(false);
        let active = ActiveConnection {
            connection_id,
            sender: tx.clone(),
            close_tx,
        };

        match self.senders.entry(device_id) {
            dashmap::mapref::entry::Entry::Occupied(mut entry) => {
                let entry = entry.get_mut();
                let replay = {
                    let mut outbox = entry.outbox.lock().expect("outbox mutex poisoned");
                    if !outbox.try_on_peer_ack(last_ack) {
                        return Err(HubError::InvalidPeerAck {
                            ack: last_ack,
                            max: outbox.last_issued_seq(),
                        }
                        .into());
                    }
                    outbox.replay_from(0)
                };
                if let Some(previous) = entry.active.replace(active.clone()) {
                    let _ = previous.close_tx.send(true);
                }
                for frame in replay {
                    let _ = tx.send(OutboundFrame { frame });
                }
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                let mut outbox = ahand_hub_core::Outbox::new(10_000);
                if !outbox.try_on_peer_ack(last_ack) {
                    return Err(HubError::InvalidPeerAck {
                        ack: last_ack,
                        max: outbox.last_issued_seq(),
                    }
                    .into());
                }
                entry.insert(ConnectionEntry {
                    active: Some(active),
                    outbox: Mutex::new(outbox),
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
        let (seq, frame) = {
            let entry = self
                .senders
                .get_mut(device_id)
                .ok_or_else(|| HubError::DeviceOffline(device_id.into()))?;
            let mut outbox = entry.outbox.lock().expect("outbox mutex poisoned");
            let seq = outbox.reserve_seq();
            envelope.seq = seq;
            envelope.ack = outbox.local_ack();
            let frame = envelope.encode_to_vec();
            outbox.store(seq, frame.clone());
            (seq, frame)
        };

        loop {
            let (sender, connection_id) = {
                let entry = self
                    .senders
                    .get(device_id)
                    .ok_or_else(|| HubError::DeviceOffline(device_id.into()))?;
                let active = entry
                    .active
                    .as_ref()
                    .ok_or_else(|| HubError::DeviceOffline(device_id.into()))?;
                (active.sender.clone(), active.connection_id)
            };
            if sender
                .send(OutboundFrame {
                    frame: frame.clone(),
                })
                .is_err()
            {
                self.clear_current_sender(device_id, connection_id);
                if !self.has_active(device_id) {
                    if let Some(entry) = self.senders.get_mut(device_id) {
                        let mut outbox = entry.outbox.lock().expect("outbox mutex poisoned");
                        outbox.remove(seq);
                    }
                    return Err(HubError::DeviceOffline(device_id.into()).into());
                }
                continue;
            }
            return Ok(());
        }
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

    pub(crate) fn has_seen_inbound(&self, device_id: &str, seq: u64) -> bool {
        if seq == 0 {
            return false;
        }
        self.senders
            .get(device_id)
            .map(|entry| {
                let outbox = entry.outbox.lock().expect("outbox mutex poisoned");
                outbox.local_ack() >= seq
            })
            .unwrap_or(false)
    }

    pub(crate) fn observe_ack(&self, device_id: &str, ack: u64) -> anyhow::Result<()> {
        if ack == 0 {
            return Ok(());
        }
        let should_cleanup = if let Some(entry) = self.senders.get_mut(device_id) {
            let mut outbox = entry.outbox.lock().expect("outbox mutex poisoned");
            if !outbox.try_on_peer_ack(ack) {
                return Err(HubError::InvalidPeerAck {
                    ack,
                    max: outbox.last_issued_seq(),
                }
                .into());
            }
            entry.active.is_none() && outbox.is_empty()
        } else {
            false
        };
        if should_cleanup {
            self.senders.remove(device_id);
        }
        Ok(())
    }

    pub(crate) fn observe_inbound(
        &self,
        device_id: &str,
        seq: u64,
        ack: u64,
    ) -> anyhow::Result<()> {
        let should_cleanup = if let Some(entry) = self.senders.get_mut(device_id) {
            let mut outbox = entry.outbox.lock().expect("outbox mutex poisoned");
            if seq > 0 {
                outbox.on_recv(seq);
            }
            if ack > 0 && !outbox.try_on_peer_ack(ack) {
                return Err(HubError::InvalidPeerAck {
                    ack,
                    max: outbox.last_issued_seq(),
                }
                .into());
            }
            entry.active.is_none() && outbox.is_empty()
        } else {
            false
        };
        if should_cleanup {
            self.senders.remove(device_id);
        }
        Ok(())
    }

    pub(crate) async fn unregister(
        &self,
        device_id: &str,
        connection_id: uuid::Uuid,
    ) -> anyhow::Result<bool> {
        if let Some(mut entry) = self.senders.get_mut(device_id)
            && entry
                .active
                .as_ref()
                .map(|active| active.connection_id == connection_id)
                .unwrap_or(false)
        {
            if let Some(active) = entry.active.take() {
                let _ = active.close_tx.send(true);
            }
            let should_remove = {
                let outbox = entry.outbox.lock().expect("outbox mutex poisoned");
                outbox.is_empty()
            };
            drop(entry);
            if should_remove {
                self.senders.remove(device_id);
            }
            return Ok(true);
        }
        Ok(false)
    }

    fn has_active(&self, device_id: &str) -> bool {
        self.senders
            .get(device_id)
            .map(|entry| entry.active.is_some())
            .unwrap_or(false)
    }

    fn clear_current_sender(&self, device_id: &str, connection_id: uuid::Uuid) {
        let should_cleanup = if let Some(mut entry) = self.senders.get_mut(device_id) {
            if entry
                .active
                .as_ref()
                .map(|active| active.connection_id == connection_id)
                .unwrap_or(false)
            {
                entry.active = None;
            }
            entry.active.is_none()
                && entry
                    .outbox
                    .lock()
                    .expect("outbox mutex poisoned")
                    .is_empty()
        } else {
            false
        };
        if should_cleanup {
            self.senders.remove(device_id);
        }
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
    let mut heartbeat_task: Option<tokio::task::JoinHandle<()>> = None;
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
        }
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
                        },
                    )),
                    ..Default::default()
                }
                .encode_to_vec()
                .into(),
            ))
            .await?;
        let device_id = envelope.device_id.clone();
        let hostname = hello.hostname.clone();
        let (connection_id, mut outbound_rx, close_rx) = state
            .connections
            .register(device_id.clone(), hello.last_ack)?;
        let (control_tx, mut control_rx) = mpsc::unbounded_channel::<WsMessage>();
        let last_inbound_at = Arc::new(AsyncMutex::new(tokio::time::Instant::now()));
        active_connection = Some((device_id.clone(), connection_id));
        state.devices.mark_online(&device_id, "ws").await?;
        state.jobs.handle_device_connected(&device_id).await?;
        if let Err(err) = state.events.emit_device_online(&device_id, &hostname).await {
            tracing::warn!(device_id = %device_id, error = %err, "failed to write device.online audit");
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

        if state.device_heartbeat_interval_ms > 0 && state.device_heartbeat_timeout_ms > 0 {
            let heartbeat_interval = state.device_heartbeat_interval_ms;
            let heartbeat_timeout = std::time::Duration::from_millis(state.device_heartbeat_timeout_ms);
            let mut heartbeat_close_rx = close_rx.clone();
            let mut heartbeat_local_close_rx = local_close_rx.clone();
            let heartbeat_last_inbound_at = last_inbound_at.clone();
            let heartbeat_control_tx = control_tx.clone();
            let heartbeat_local_close_tx = local_close_tx.clone();
            heartbeat_task = Some(tokio::spawn(async move {
                loop {
                    tokio::select! {
                        biased;
                        _ = heartbeat_close_rx.changed() => break,
                        _ = heartbeat_local_close_rx.changed() => break,
                        _ = tokio::time::sleep(std::time::Duration::from_millis(heartbeat_interval)) => {
                            let elapsed = heartbeat_last_inbound_at.lock().await.elapsed();
                            if elapsed >= heartbeat_timeout {
                                let _ = heartbeat_local_close_tx.send(true);
                                break;
                            }
                            if heartbeat_control_tx
                                .send(WsMessage::Ping(Vec::new().into()))
                                .is_err()
                            {
                                let _ = heartbeat_local_close_tx.send(true);
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
                    state.jobs.handle_device_frame(&device_id, &frame).await?;
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
    if let Some(task) = heartbeat_task.take() {
        task.abort();
    }
    if let Some(reservation) = bootstrap_reservation.take() {
        if let Err(err) = state.bootstrap_tokens.release(&reservation).await {
            tracing::warn!(
                device_id = %reservation.device_id,
                error = %err,
                "failed to release bootstrap reservation"
            );
        }
    }
    if let Some((device_id, connection_id)) = active_connection.take()
        && state
            .connections
            .unregister(&device_id, connection_id)
            .await?
    {
        state.devices.mark_offline(&device_id).await?;
        state.jobs.handle_device_disconnected(&device_id).await?;
        if let Err(err) = state.events.emit_device_offline(&device_id).await {
            tracing::warn!(device_id = %device_id, error = %err, "failed to write device.offline audit");
        }
    }

    run_result
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
    use prost::Message;
    use std::time::Duration;

    use super::ConnectionRegistry;

    #[tokio::test]
    async fn register_replays_only_messages_after_last_ack() {
        let registry = ConnectionRegistry::default();
        let (_connection_id, mut initial_rx, _close_rx) =
            registry.register("device-1".into(), 0).unwrap();
        let transport = tokio::spawn(async move {
            while let Some(outbound) = initial_rx.recv().await {
                let _ = outbound;
            }
        });

        registry
            .send(
                "device-1",
                ahand_protocol::Envelope {
                    device_id: "device-1".into(),
                    msg_id: "job-1".into(),
                    payload: Some(ahand_protocol::envelope::Payload::JobRequest(
                        ahand_protocol::JobRequest {
                            job_id: "job-1".into(),
                            tool: "echo".into(),
                            args: vec!["one".into()],
                            cwd: String::new(),
                            env: Default::default(),
                            timeout_ms: 30_000,
                        },
                    )),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        registry
            .send(
                "device-1",
                ahand_protocol::Envelope {
                    device_id: "device-1".into(),
                    msg_id: "job-2".into(),
                    payload: Some(ahand_protocol::envelope::Payload::JobRequest(
                        ahand_protocol::JobRequest {
                            job_id: "job-2".into(),
                            tool: "echo".into(),
                            args: vec!["two".into()],
                            cwd: String::new(),
                            env: Default::default(),
                            timeout_ms: 30_000,
                        },
                    )),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        transport.abort();

        let (_reconnect_id, mut replay_rx, _close_rx) =
            registry.register("device-1".into(), 1).unwrap();
        let replayed = replay_rx.recv().await.expect("replayed frame should exist");
        let replayed = ahand_protocol::Envelope::decode(replayed.frame.as_slice()).unwrap();
        let Some(ahand_protocol::envelope::Payload::JobRequest(job)) = replayed.payload else {
            panic!("expected replayed job request");
        };
        assert_eq!(job.job_id, "job-2");
        assert_eq!(replayed.seq, 2);
        assert_eq!(replayed.ack, 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), replay_rx.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn inbound_seq_updates_outbound_ack() {
        let registry = ConnectionRegistry::default();
        let (_connection_id, mut rx, _close_rx) = registry.register("device-1".into(), 0).unwrap();
        registry.observe_inbound("device-1", 7, 0).unwrap();

        let transport = tokio::spawn(async move {
            while let Some(outbound) = rx.recv().await {
                let _ = outbound;
            }
        });

        registry
            .send(
                "device-1",
                ahand_protocol::Envelope {
                    device_id: "device-1".into(),
                    msg_id: "job-1".into(),
                    payload: Some(ahand_protocol::envelope::Payload::JobRequest(
                        ahand_protocol::JobRequest {
                            job_id: "job-1".into(),
                            tool: "echo".into(),
                            args: vec!["hello".into()],
                            cwd: String::new(),
                            env: Default::default(),
                            timeout_ms: 30_000,
                        },
                    )),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let frame = registry.senders.get("device-1").unwrap();
        let outbound = frame.outbox.lock().unwrap().replay_from(0).pop().unwrap();
        let outbound = ahand_protocol::Envelope::decode(outbound.as_slice()).unwrap();
        assert_eq!(outbound.seq, 1);
        assert_eq!(outbound.ack, 7);
        transport.abort();
    }

    #[tokio::test]
    async fn unregister_removes_idle_entries() {
        let registry = ConnectionRegistry::default();
        let (connection_id, rx, _close_rx) = registry.register("device-1".into(), 0).unwrap();
        drop(rx);

        let removed = registry
            .unregister("device-1", connection_id)
            .await
            .unwrap();

        assert!(removed);
        assert!(registry.senders.is_empty());
    }
}
