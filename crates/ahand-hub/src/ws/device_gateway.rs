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
    senders: DashMap<String, mpsc::UnboundedSender<ahand_protocol::Envelope>>,
}

impl ConnectionRegistry {
    pub fn register(&self, device_id: String) -> mpsc::UnboundedReceiver<ahand_protocol::Envelope> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.senders.insert(device_id, tx);
        rx
    }

    pub fn send(&self, device_id: &str, envelope: ahand_protocol::Envelope) -> anyhow::Result<()> {
        let sender = self
            .senders
            .get(device_id)
            .ok_or_else(|| anyhow::anyhow!("device {device_id} is not connected"))?;
        sender
            .send(envelope)
            .map_err(|_| anyhow::anyhow!("device {device_id} connection closed"))
    }

    pub async fn unregister(&self, device_id: &str) -> anyhow::Result<()> {
        self.senders.remove(device_id);
        Ok(())
    }
}

pub async fn handle_device_socket(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket| async move {
        if let Err(err) = run_device_socket(socket, state).await {
            tracing::warn!(error = %err, "device socket ended with error");
        }
    })
}

async fn run_device_socket(socket: WebSocket, state: AppState) -> anyhow::Result<()> {
    let (mut sender, mut receiver) = socket.split();
    let Some(Ok(WsMessage::Binary(first_frame))) = receiver.next().await else {
        return Ok(());
    };

    let envelope = ahand_protocol::Envelope::decode(first_frame.as_ref())?;
    let hello = match envelope.payload {
        Some(ahand_protocol::envelope::Payload::Hello(hello)) => hello,
        _ => anyhow::bail!("expected hello envelope"),
    };

    crate::auth::verify_device_hello(
        &envelope.device_id,
        &hello,
        state.device_bootstrap_token.as_str(),
    )?;
    state.devices.upsert_from_hello(&envelope.device_id, &hello)?;
    state
        .events
        .emit_device_online(&envelope.device_id, &hello.hostname)
        .await?;

    let device_id = envelope.device_id.clone();
    let mut outbound_rx = state.connections.register(device_id.clone());
    let send_task = tokio::spawn(async move {
        while let Some(envelope) = outbound_rx.recv().await {
            if sender
                .send(WsMessage::Binary(envelope.encode_to_vec().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

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

    state.connections.unregister(&device_id).await?;
    state.devices.mark_offline(&device_id)?;
    state.events.emit_device_offline(&device_id).await?;
    send_task.abort();
    Ok(())
}
