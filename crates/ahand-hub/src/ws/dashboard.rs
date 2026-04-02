use ahand_hub_core::auth::Role;
use axum::{
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::HeaderMap,
    http::StatusCode,
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast::error::RecvError;

use crate::auth::authenticate_token;
use crate::events::DashboardEvent;
use crate::state::AppState;

pub async fn handle_dashboard_socket(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let token = session_cookie_token(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    validate_same_origin(&headers)?;
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
                    Err(RecvError::Lagged(_)) => {
                        let payload = serde_json::to_string(&resync_event("lagged"))?;
                        sender.send(Message::Text(payload.into())).await?;
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        }
    }

    Ok(())
}

fn session_cookie_token(headers: &HeaderMap) -> Option<&str> {
    let cookies = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    cookies
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("ahand_hub_session="))
}

fn validate_same_origin(headers: &HeaderMap) -> Result<(), StatusCode> {
    let origin = headers
        .get(axum::http::header::ORIGIN)
        .ok_or(StatusCode::FORBIDDEN)?
        .to_str()
        .map_err(|_| StatusCode::FORBIDDEN)?;
    let host = headers
        .get(axum::http::header::HOST)
        .ok_or(StatusCode::FORBIDDEN)?
        .to_str()
        .map_err(|_| StatusCode::FORBIDDEN)?;

    let parsed = url::Url::parse(origin).map_err(|_| StatusCode::FORBIDDEN)?;
    match parsed.port_or_known_default() {
        Some(port) if format!("{}:{port}", parsed.host_str().unwrap_or_default()) == host => Ok(()),
        None if parsed.host_str().unwrap_or_default() == host => Ok(()),
        _ => Err(StatusCode::FORBIDDEN),
    }
}

fn resync_event(reason: &str) -> DashboardEvent {
    DashboardEvent {
        event: "system.resync".into(),
        resource_type: "system".into(),
        resource_id: "dashboard".into(),
        actor: "hub".into(),
        detail: serde_json::json!({ "reason": reason }),
        timestamp: chrono::Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lagged_broadcasts_turn_into_resync_events() {
        let event = resync_event("lagged");

        assert_eq!(event.event, "system.resync");
        assert_eq!(event.resource_type, "system");
        assert_eq!(event.resource_id, "dashboard");
        assert_eq!(event.actor, "hub");
        assert_eq!(event.detail["reason"], "lagged");
    }
}
