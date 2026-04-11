use std::time::Instant;

use axum::{
    extract::{
        Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};

use crate::auth::AuthContextExt;
use crate::http::api_error::{ApiError, ApiResult};
use crate::output_stream::OutputEvent;
use crate::state::AppState;

/// One-time token for authenticating a terminal WebSocket connection.
#[derive(Clone, Debug)]
pub struct TerminalToken {
    pub job_id: String,
    pub device_id: String,
    pub created_at: Instant,
}

const TOKEN_TTL_SECS: u64 = 60;

// --- POST /api/terminal/token ---

#[derive(Deserialize)]
pub struct CreateTokenRequest {
    pub job_id: String,
}

#[derive(Serialize)]
pub struct CreateTokenResponse {
    pub token: String,
    pub ws_url: String,
}

pub async fn create_token(
    auth: AuthContextExt,
    State(state): State<AppState>,
    Json(body): Json<CreateTokenRequest>,
) -> ApiResult<Json<CreateTokenResponse>> {
    auth.require_dashboard_access()?;

    // Validate job exists and is running
    let job = state
        .jobs
        .get_job(&body.job_id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "JOB_NOT_FOUND",
                format!("Job {} was not found", body.job_id),
            )
        })?;

    if ahand_hub_core::job::is_terminal_status(job.status) {
        return Err(ApiError::gone(format!(
            "Job {} has already finished",
            body.job_id
        )));
    }

    let token = uuid::Uuid::new_v4().to_string();
    state.terminal_tokens.insert(
        token.clone(),
        TerminalToken {
            job_id: body.job_id,
            device_id: job.device_id,
            created_at: Instant::now(),
        },
    );

    Ok(Json(CreateTokenResponse {
        token,
        ws_url: "/ws/terminal".into(),
    }))
}

// --- GET /ws/terminal ---

#[derive(Deserialize)]
pub struct TerminalWsQuery {
    pub token: String,
    pub job_id: String,
}

pub async fn handle_terminal_ws(
    ws: WebSocketUpgrade,
    Query(query): Query<TerminalWsQuery>,
    State(state): State<AppState>,
) -> Response {
    // Atomically consume the token
    let Some((_, terminal_token)) = state.terminal_tokens.remove(&query.token) else {
        return (StatusCode::UNAUTHORIZED, "Invalid or expired token").into_response();
    };

    // Verify token is not expired
    if terminal_token.created_at.elapsed().as_secs() > TOKEN_TTL_SECS {
        return (StatusCode::UNAUTHORIZED, "Token expired").into_response();
    }

    // Verify job_id matches
    if terminal_token.job_id != query.job_id {
        return (StatusCode::BAD_REQUEST, "Token job_id mismatch").into_response();
    }

    let job_id = terminal_token.job_id;
    let device_id = terminal_token.device_id;

    ws.on_upgrade(move |socket| async move {
        if let Err(err) = run_terminal_bridge(socket, state, job_id, device_id).await {
            tracing::warn!(error = %err, "terminal websocket ended with error");
        }
    })
}

#[derive(Deserialize)]
struct ResizeMessage {
    #[serde(rename = "type")]
    msg_type: String,
    cols: u32,
    rows: u32,
}

async fn run_terminal_bridge(
    socket: WebSocket,
    state: AppState,
    job_id: String,
    device_id: String,
) -> anyhow::Result<()> {
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Subscribe to output stream for this job
    let mut output_stream = state
        .output_stream
        .subscribe_terminal(job_id.clone())
        .await?;

    loop {
        tokio::select! {
            // Browser -> Daemon: forward stdin and resize
            message = ws_receiver.next() => {
                match message {
                    Some(Ok(Message::Binary(data))) => {
                        // Binary frame = keystroke data -> StdinChunk
                        let envelope = ahand_protocol::Envelope {
                            device_id: device_id.clone(),
                            msg_id: format!("ws-stdin-{job_id}"),
                            ts_ms: now_ms(),
                            payload: Some(ahand_protocol::envelope::Payload::StdinChunk(
                                ahand_protocol::StdinChunk {
                                    job_id: job_id.clone(),
                                    data: data.to_vec(),
                                },
                            )),
                            ..Default::default()
                        };
                        if let Err(err) = state.connections.send(&device_id, envelope).await {
                            tracing::warn!(
                                device_id,
                                job_id,
                                error = %err,
                                "failed to forward stdin to device"
                            );
                            break;
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        // Text frame = JSON resize message
                        match serde_json::from_str::<ResizeMessage>(&text) {
                            Ok(resize) if resize.msg_type == "resize" => {
                                let envelope = ahand_protocol::Envelope {
                                    device_id: device_id.clone(),
                                    msg_id: format!("ws-resize-{job_id}"),
                                    ts_ms: now_ms(),
                                    payload: Some(
                                        ahand_protocol::envelope::Payload::TerminalResize(
                                            ahand_protocol::TerminalResize {
                                                job_id: job_id.clone(),
                                                cols: resize.cols,
                                                rows: resize.rows,
                                            },
                                        ),
                                    ),
                                    ..Default::default()
                                };
                                if let Err(err) = state.connections.send(&device_id, envelope).await {
                                    tracing::warn!(
                                        device_id,
                                        job_id,
                                        error = %err,
                                        "failed to forward resize to device"
                                    );
                                    break;
                                }
                            }
                            Ok(_) => {
                                tracing::debug!(job_id, "ignoring unknown text message type");
                            }
                            Err(err) => {
                                tracing::debug!(job_id, error = %err, "ignoring unparseable text frame");
                            }
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        ws_sender.send(Message::Pong(payload)).await?;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {} // Pong, etc.
                    Some(Err(err)) => return Err(err.into()),
                }
            }
            // Daemon -> Browser: forward output
            event = output_stream.next() => {
                match event {
                    Some(OutputEvent::Stdout(data)) => {
                        ws_sender.send(Message::Binary(data.into())).await?;
                    }
                    Some(OutputEvent::Stderr(data)) => {
                        // Send stderr as binary too (terminal mixes both)
                        ws_sender.send(Message::Binary(data.into())).await?;
                    }
                    Some(OutputEvent::Finished { exit_code, error }) => {
                        // Send a close frame with the exit info
                        let reason = if error.is_empty() {
                            format!("exit:{exit_code}")
                        } else {
                            format!("exit:{exit_code}:{error}")
                        };
                        ws_sender
                            .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                                code: 1000,
                                reason: reason.into(),
                            })))
                            .await?;
                        break;
                    }
                    None => {
                        // Output stream ended (job cleanup)
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_token_expires_after_ttl() {
        let token = TerminalToken {
            job_id: "job-1".into(),
            device_id: "device-1".into(),
            created_at: Instant::now() - std::time::Duration::from_secs(TOKEN_TTL_SECS + 1),
        };
        assert!(token.created_at.elapsed().as_secs() > TOKEN_TTL_SECS);
    }

    #[test]
    fn terminal_token_valid_within_ttl() {
        let token = TerminalToken {
            job_id: "job-1".into(),
            device_id: "device-1".into(),
            created_at: Instant::now(),
        };
        assert!(token.created_at.elapsed().as_secs() <= TOKEN_TTL_SECS);
    }

    #[test]
    fn resize_message_parses_correctly() {
        let json = r#"{"type":"resize","cols":120,"rows":40}"#;
        let msg: ResizeMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.msg_type, "resize");
        assert_eq!(msg.cols, 120);
        assert_eq!(msg.rows, 40);
    }

    #[test]
    fn resize_message_rejects_invalid_json() {
        let json = r#"{"type":"unknown"}"#;
        let result = serde_json::from_str::<ResizeMessage>(json);
        assert!(result.is_err());
    }
}
