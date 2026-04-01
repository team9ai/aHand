use ahand_hub_core::auth::Role;
use axum::{
    extract::{
        Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;

use crate::auth::authenticate_token;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct DashboardSocketQuery {
    pub token: Option<String>,
}

pub async fn handle_dashboard_socket(
    ws: WebSocketUpgrade,
    Query(query): Query<DashboardSocketQuery>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let token = query.token.as_deref().ok_or(StatusCode::UNAUTHORIZED)?;
    let claims = authenticate_token(&state, token).map_err(|_| StatusCode::UNAUTHORIZED)?;
    match claims.role {
        Role::Admin | Role::DashboardUser => {}
        _ => return Err(StatusCode::FORBIDDEN),
    }

    Ok(ws.on_upgrade(move |socket| async move {
        if let Err(err) = run_dashboard_socket(socket, state).await {
            tracing::warn!(error = %err, "dashboard websocket ended with error");
        }
    }))
}

async fn run_dashboard_socket(socket: WebSocket, state: AppState) -> anyhow::Result<()> {
    let (mut sender, mut receiver) = socket.split();
    let mut events = state.events.subscribe();

    loop {
        tokio::select! {
            message = receiver.next() => {
                match message {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(payload))) => sender.send(Message::Pong(payload)).await?,
                    Some(Ok(_)) => {}
                    Some(Err(err)) => return Err(err.into()),
                }
            }
            event = events.recv() => {
                match event {
                    Ok(event) => {
                        let payload = serde_json::to_string(&event)?;
                        sender.send(Message::Text(payload.into())).await?;
                    }
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                }
            }
        }
    }

    Ok(())
}
