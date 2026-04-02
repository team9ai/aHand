use std::sync::Mutex;

use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio::sync::{mpsc, oneshot};

use ahand_hub_core::HubError;
use axum::extract::State;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::response::Response;

use crate::state::AppState;

#[derive(Default)]
pub struct ConnectionRegistry {
    senders: DashMap<String, ConnectionEntry>,
}

struct ConnectionEntry {
    connection_id: Option<uuid::Uuid>,
    sender: Option<mpsc::UnboundedSender<OutboundFrame>>,
    outbox: Mutex<ahand_hub_core::Outbox>,
}

pub(crate) struct OutboundFrame {
    pub(crate) frame: Vec<u8>,
    pub(crate) delivered: Option<oneshot::Sender<Result<(), ()>>>,
}

impl ConnectionRegistry {
    pub(crate) fn register(
        &self,
        device_id: String,
        last_ack: u64,
    ) -> (uuid::Uuid, mpsc::UnboundedReceiver<OutboundFrame>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let connection_id = uuid::Uuid::new_v4();

        match self.senders.entry(device_id) {
            dashmap::mapref::entry::Entry::Occupied(mut entry) => {
                let entry = entry.get_mut();
                entry.connection_id = Some(connection_id);
                entry.sender = Some(tx.clone());
                let replay = {
                    let mut outbox = entry.outbox.lock().expect("outbox mutex poisoned");
                    outbox.on_peer_ack(last_ack);
                    outbox.replay_from(0)
                };
                for frame in replay {
                    let _ = tx.send(OutboundFrame {
                        frame,
                        delivered: None,
                    });
                }
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                let mut outbox = ahand_hub_core::Outbox::new(10_000);
                outbox.on_peer_ack(last_ack);
                entry.insert(ConnectionEntry {
                    connection_id: Some(connection_id),
                    sender: Some(tx),
                    outbox: Mutex::new(outbox),
                });
            }
        }

        (connection_id, rx)
    }

    pub(crate) async fn send(
        &self,
        device_id: &str,
        mut envelope: ahand_protocol::Envelope,
    ) -> anyhow::Result<()> {
        let (sender, connection_id, seq, frame) = {
            let entry = self
                .senders
                .get_mut(device_id)
                .ok_or_else(|| HubError::DeviceOffline(device_id.into()))?;
            let sender = entry
                .sender
                .as_ref()
                .cloned()
                .ok_or_else(|| HubError::DeviceOffline(device_id.into()))?;
            let connection_id = entry
                .connection_id
                .ok_or_else(|| HubError::DeviceOffline(device_id.into()))?;
            let mut outbox = entry.outbox.lock().expect("outbox mutex poisoned");
            let seq = outbox.reserve_seq();
            envelope.seq = seq;
            envelope.ack = outbox.local_ack();
            (sender, connection_id, seq, envelope.encode_to_vec())
        };
        let (delivery_tx, delivery_rx) = oneshot::channel();
        sender
            .send(OutboundFrame {
                frame: frame.clone(),
                delivered: Some(delivery_tx),
            })
            .map_err(|_| HubError::DeviceOffline(device_id.into()))?;

        match delivery_rx.await {
            Ok(Ok(())) => {
                let entry = self
                    .senders
                    .get_mut(device_id)
                    .ok_or_else(|| HubError::DeviceOffline(device_id.into()))?;
                if entry.connection_id != Some(connection_id) {
                    return Err(HubError::DeviceOffline(device_id.into()).into());
                }
                let mut outbox = entry.outbox.lock().expect("outbox mutex poisoned");
                outbox.store(seq, frame);
                Ok(())
            }
            _ => {
                let _ = self.unregister(device_id, connection_id).await;
                self.cleanup_idle_entry(device_id);
                Err(HubError::DeviceOffline(device_id.into()).into())
            }
        }
    }

    pub(crate) fn is_current(&self, device_id: &str, connection_id: uuid::Uuid) -> bool {
        self.senders
            .get(device_id)
            .map(|entry| entry.connection_id == Some(connection_id))
            .unwrap_or(false)
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

    pub(crate) fn observe_ack(&self, device_id: &str, ack: u64) {
        if ack == 0 {
            return;
        }
        let should_cleanup = if let Some(entry) = self.senders.get_mut(device_id) {
            let mut outbox = entry.outbox.lock().expect("outbox mutex poisoned");
            outbox.on_peer_ack(ack);
            entry.sender.is_none() && entry.connection_id.is_none() && outbox.is_empty()
        } else {
            false
        };
        if should_cleanup {
            self.senders.remove(device_id);
        }
    }

    pub(crate) fn observe_inbound(&self, device_id: &str, seq: u64, ack: u64) {
        let should_cleanup = if let Some(entry) = self.senders.get_mut(device_id) {
            let mut outbox = entry.outbox.lock().expect("outbox mutex poisoned");
            if seq > 0 {
                outbox.on_recv(seq);
            }
            if ack > 0 {
                outbox.on_peer_ack(ack);
            }
            entry.sender.is_none() && entry.connection_id.is_none() && outbox.is_empty()
        } else {
            false
        };
        if should_cleanup {
            self.senders.remove(device_id);
        }
    }

    pub(crate) async fn unregister(
        &self,
        device_id: &str,
        connection_id: uuid::Uuid,
    ) -> anyhow::Result<bool> {
        if let Some(mut entry) = self.senders.get_mut(device_id)
            && entry.connection_id == Some(connection_id)
        {
            entry.connection_id = None;
            entry.sender = None;
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

    fn cleanup_idle_entry(&self, device_id: &str) {
        let remove = self
            .senders
            .get(device_id)
            .map(|entry| {
                entry.sender.is_none()
                    && entry.connection_id.is_none()
                    && entry.outbox.lock().expect("outbox mutex poisoned").is_empty()
            })
            .unwrap_or(false);
        if remove {
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
    let mut send_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut presence_task: Option<tokio::task::JoinHandle<()>> = None;

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
            _ => anyhow::bail!("expected hello envelope"),
        };

        let verified = crate::auth::verify_device_hello(
            &envelope.device_id,
            &hello,
            state.auth.as_ref(),
            &challenge.nonce,
            state.device_bootstrap_token.as_str(),
            state.device_bootstrap_device_id.as_str(),
            state.device_hello_max_age_ms,
        )?;
        state
            .devices
            .accept_verified_hello(&envelope.device_id, &hello, &verified)
            .await?;
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
        let (connection_id, mut outbound_rx) =
            state.connections.register(device_id.clone(), hello.last_ack);
        active_connection = Some((device_id.clone(), connection_id));
        state.devices.mark_online(&device_id, "ws").await?;
        if let Err(err) = state.events.emit_device_online(&device_id, &hostname).await {
            tracing::warn!(device_id = %device_id, error = %err, "failed to write device.online audit");
        }

        if state.device_presence_refresh_ms > 0 {
            let devices = state.devices.clone();
            let refresh_ms = state.device_presence_refresh_ms;
            let refresh_device_id = device_id.clone();
            presence_task = Some(tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(refresh_ms)).await;
                    if let Err(err) = devices.mark_online(&refresh_device_id, "ws").await {
                        tracing::warn!(device_id = %refresh_device_id, error = %err, "failed to refresh device presence");
                        break;
                    }
                }
            }));
        }

        send_task = Some(tokio::spawn(async move {
            while let Some(outbound) = outbound_rx.recv().await {
                if sender
                    .send(WsMessage::Binary(outbound.frame.into()))
                    .await
                    .is_err()
                {
                    if let Some(delivered) = outbound.delivered {
                        let _ = delivered.send(Err(()));
                    }
                    break;
                }
                if let Some(delivered) = outbound.delivered {
                    let _ = delivered.send(Ok(()));
                }
            }
        }));

        while let Some(message) = receiver.next().await {
            let message = message?;
            if !state.connections.is_current(&device_id, connection_id) {
                break;
            }
            match message {
                WsMessage::Binary(frame) => {
                    state.jobs.handle_device_frame(&device_id, &frame).await?;
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
    if let Some((device_id, connection_id)) = active_connection.take()
        && state
            .connections
            .unregister(&device_id, connection_id)
            .await?
    {
        state.devices.mark_offline(&device_id).await?;
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

#[cfg(test)]
mod tests {
    use prost::Message;
    use std::time::Duration;

    use super::ConnectionRegistry;

    #[tokio::test]
    async fn register_replays_only_messages_after_last_ack() {
        let registry = ConnectionRegistry::default();
        let (_connection_id, mut initial_rx) = registry.register("device-1".into(), 0);
        let transport = tokio::spawn(async move {
            while let Some(outbound) = initial_rx.recv().await {
                if let Some(delivered) = outbound.delivered {
                    let _ = delivered.send(Ok(()));
                }
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

        let (_reconnect_id, mut replay_rx) = registry.register("device-1".into(), 1);
        let replayed = replay_rx.recv().await.expect("replayed frame should exist");
        let replayed = ahand_protocol::Envelope::decode(replayed.frame.as_slice()).unwrap();
        let Some(ahand_protocol::envelope::Payload::JobRequest(job)) = replayed.payload else {
            panic!("expected replayed job request");
        };
        assert_eq!(job.job_id, "job-2");
        assert_eq!(replayed.seq, 2);
        assert_eq!(replayed.ack, 0);
        assert!(tokio::time::timeout(Duration::from_millis(20), replay_rx.recv())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn inbound_seq_updates_outbound_ack() {
        let registry = ConnectionRegistry::default();
        let (_connection_id, mut rx) = registry.register("device-1".into(), 0);
        registry.observe_inbound("device-1", 7, 0);

        let transport = tokio::spawn(async move {
            while let Some(outbound) = rx.recv().await {
                if let Some(delivered) = outbound.delivered {
                    let _ = delivered.send(Ok(()));
                }
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
        let (connection_id, rx) = registry.register("device-1".into(), 0);
        drop(rx);

        let removed = registry.unregister("device-1", connection_id).await.unwrap();

        assert!(removed);
        assert!(registry.senders.is_empty());
    }
}
