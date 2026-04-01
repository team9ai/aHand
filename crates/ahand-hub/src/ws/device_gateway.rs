use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio::sync::mpsc;

use axum::extract::State;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::response::Response;

use crate::state::AppState;

#[derive(Default)]
pub struct ConnectionRegistry {
    senders: DashMap<String, ConnectionEntry>,
}

struct ConnectionEntry {
    connection_id: uuid::Uuid,
    sender: mpsc::UnboundedSender<ahand_protocol::Envelope>,
}

impl ConnectionRegistry {
    pub fn register(
        &self,
        device_id: String,
    ) -> (
        uuid::Uuid,
        mpsc::UnboundedReceiver<ahand_protocol::Envelope>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let connection_id = uuid::Uuid::new_v4();
        self.senders.insert(
            device_id,
            ConnectionEntry {
                connection_id,
                sender: tx,
            },
        );
        (connection_id, rx)
    }

    pub fn send(&self, device_id: &str, envelope: ahand_protocol::Envelope) -> anyhow::Result<()> {
        let sender = self
            .senders
            .get(device_id)
            .ok_or_else(|| anyhow::anyhow!("device {device_id} is not connected"))?;
        sender
            .sender
            .send(envelope)
            .map_err(|_| anyhow::anyhow!("device {device_id} connection closed"))
    }

    pub async fn unregister(
        &self,
        device_id: &str,
        connection_id: uuid::Uuid,
    ) -> anyhow::Result<bool> {
        let should_remove = self
            .senders
            .get(device_id)
            .is_some_and(|entry| entry.connection_id == connection_id);
        if should_remove {
            self.senders.remove(device_id);
            Ok(true)
        } else {
            Ok(false)
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
    let (connection_id, mut outbound_rx) = state.connections.register(device_id.clone());
    active_connection = Some((device_id.clone(), connection_id));
    if let Err(err) = state
        .events
        .emit_device_online(&envelope.device_id, &hello.hostname)
        .await
    {
        tracing::warn!(device_id = %envelope.device_id, error = %err, "failed to write device.online audit");
    }

    send_task = Some(tokio::spawn(async move {
        while let Some(envelope) = outbound_rx.recv().await {
            if sender
                .send(WsMessage::Binary(envelope.encode_to_vec().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    }));

    while let Some(message) = receiver.next().await {
        let message = message?;
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
    if let Some((device_id, connection_id)) = active_connection.take() {
        if state
            .connections
            .unregister(&device_id, connection_id)
            .await?
        {
            state.devices.mark_offline(&device_id).await?;
            if let Err(err) = state.events.emit_device_offline(&device_id).await {
                tracing::warn!(device_id = %device_id, error = %err, "failed to write device.offline audit");
            }
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
