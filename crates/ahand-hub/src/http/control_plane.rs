//! Control-plane REST + SSE surface: `/api/control/*`.
//!
//! This is the API surface the team9 im-worker calls via the
//! `@ahand/sdk` client to dispatch jobs on behalf of a user to one of
//! their devices. Endpoints:
//!
//!   * `POST /api/control/jobs`            — dispatch a job
//!   * `GET  /api/control/jobs/{id}/stream` — SSE event stream
//!   * `POST /api/control/jobs/{id}/cancel` — best-effort cancel
//!   * `POST /api/control/browser`         — synchronous browser command
//!   * `POST /api/control/files`           — synchronous file operation
//!
//! Auth: **control-plane JWT** (`token_type = ControlPlane`) only —
//! device JWTs are rejected by [`verify_control_plane_jwt`]. The JWT's
//! `external_user_id` is the ownership anchor: a request only succeeds
//! if the device's `external_user_id` matches the token's.
//!
//! Rate limiting: per-`external_user_id` token bucket (see
//! [`crate::state::default_control_plane_rate_limiter`]).
//!
//! Idempotency: `POST /api/control/jobs` accepts an optional
//! `correlation_id`. A second POST with the same
//! `(external_user_id, correlation_id)` pair while the original job is
//! still live returns the original `job_id` without re-dispatching.
//! `POST /api/control/browser` accepts the field in its wire schema
//! but does not currently dedupe — `correlation_id` is forwarded into
//! `BrowserCommandInput` but discarded by `browser_service::execute`.
//! Hub-layer dedupe for the browser endpoint is tracked as a
//! follow-up; today's worker semantics rely on best-effort retries
//! via fresh requests.

use std::convert::Infallible;
use std::time::Duration;

use ahand_hub_core::auth::ControlPlaneJwtClaims;
use ahand_hub_core::traits::DeviceAdminStore;
use axum::Extension;
use axum::Json;
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderValue, StatusCode, header::AUTHORIZATION};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;

use crate::browser_service::{self, BrowserCommandInput};
use crate::control_jobs::ControlJobEvent;
use crate::http::api_error::{ApiError, ApiResult};
use crate::http::browser::map_service_error;
use crate::state::AppState;

/// Mount the control-plane router. The caller passes the shared
/// `AppState` so the JWT middleware can verify tokens against the
/// hub's JWT secret.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/api/control/jobs", post(create_job))
        .route("/api/control/jobs/{id}/stream", get(stream_job))
        .route("/api/control/jobs/{id}/cancel", post(cancel_job))
        .route("/api/control/browser", post(browser_command_control))
        .route("/api/control/files", post(control_files))
        .layer(middleware::from_fn_with_state(
            state,
            require_control_plane_jwt,
        ))
}

/// Axum middleware that verifies the `Authorization: Bearer <jwt>`
/// header against the hub's control-plane JWT secret and stashes the
/// decoded claims into request extensions.
async fn require_control_plane_jwt(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ControlError> {
    let Some(header) = req.headers().get(AUTHORIZATION) else {
        return Err(ControlError::Unauthorized);
    };
    let token = header_bearer(header).ok_or(ControlError::Unauthorized)?;
    let claims = state
        .auth
        .verify_control_plane_jwt(&token)
        .map_err(|_| ControlError::Unauthorized)?;
    req.extensions_mut().insert(claims);
    Ok(next.run(req).await)
}

fn header_bearer(value: &HeaderValue) -> Option<String> {
    value
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(String::from)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateJobRequest {
    pub device_id: String,
    pub tool: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub interactive: bool,
    #[serde(default)]
    pub correlation_id: Option<String>,
}

/// Default job timeout when the SDK doesn't pass one. Matches the
/// dashboard default (5 minutes) — a daemon that never responds will
/// still release hub resources after this much time has elapsed.
const DEFAULT_JOB_TIMEOUT_MS: u64 = 5 * 60 * 1000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateJobResponse {
    pub job_id: String,
}

async fn create_job(
    State(state): State<AppState>,
    Extension(claims): Extension<ControlPlaneJwtClaims>,
    body: Result<Json<CreateJobRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<CreateJobResponse>), ControlError> {
    // Validate the scope claim before any DB access.
    if claims.scope != "jobs:execute" {
        return Err(ControlError::Forbidden);
    }

    let Json(req) = body.map_err(|_| ControlError::BadRequest("invalid JSON body".into()))?;
    if req.tool.trim().is_empty() {
        return Err(ControlError::BadRequest("tool must not be empty".into()));
    }
    if req.device_id.is_empty() {
        return Err(ControlError::BadRequest(
            "deviceId must not be empty".into(),
        ));
    }

    // Rate-limit BEFORE the expensive ownership lookup so a storm of
    // bogus POSTs can't DOS the device store.
    if state
        .control_rate_limiter
        .check_key(&claims.external_user_id)
        .is_err()
    {
        return Err(ControlError::RateLimited);
    }

    // Ownership: device must exist, be owned by the calling user, and
    // currently be online via WS.
    let device = state
        .devices
        .find_by_id(&req.device_id)
        .await
        .map_err(|err| ControlError::Internal(err.to_string()))?
        .ok_or(ControlError::DeviceNotFound)?;
    if device.external_user_id.as_deref() != Some(claims.external_user_id.as_str()) {
        // A device with no external_user_id (legacy / dashboard-only)
        // is treated as "not yours" for control-plane purposes. A
        // device owned by a *different* user is likewise 403. We map
        // both to 403 deliberately — 404 would leak device-id
        // existence across user boundaries.
        return Err(ControlError::Forbidden);
    }
    // Enforce device_ids allowlist if the token is scoped to specific devices.
    if let Some(allowed) = &claims.device_ids
        && !allowed.contains(&req.device_id)
    {
        return Err(ControlError::Forbidden);
    }
    if !state.connections.is_online(&device.id) {
        return Err(ControlError::DeviceOffline);
    }

    // Idempotency: only honour correlation_id when it's non-empty.
    if let Some(cid) = req.correlation_id.as_deref()
        && !cid.is_empty()
        && let Some(existing) = state
            .control_jobs
            .find_by_correlation(&claims.external_user_id, cid)
    {
        return Ok((StatusCode::OK, Json(CreateJobResponse { job_id: existing })));
    }

    let job_id = ulid::Ulid::new().to_string();
    state.control_jobs.register(
        job_id.clone(),
        device.id.clone(),
        claims.external_user_id.clone(),
        req.correlation_id.clone().filter(|cid| !cid.is_empty()),
    );

    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_JOB_TIMEOUT_MS);
    let envelope = ahand_protocol::Envelope {
        device_id: device.id.clone(),
        msg_id: format!("control-job-{job_id}"),
        ts_ms: now_ms(),
        payload: Some(ahand_protocol::envelope::Payload::JobRequest(
            ahand_protocol::JobRequest {
                job_id: job_id.clone(),
                tool: req.tool.clone(),
                args: req.args.clone(),
                cwd: req.cwd.clone().unwrap_or_default(),
                env: req.env.clone(),
                timeout_ms,
                interactive: req.interactive,
            },
        )),
        ..Default::default()
    };

    if let Err(err) = state.connections.send_envelope(&device.id, envelope).await {
        // Roll back the registry entry so a retry doesn't find a
        // phantom job. The broadcast channel has no subscribers yet,
        // so this is lossless.
        state.control_jobs.finalize(
            &job_id,
            ControlJobEvent::Error {
                code: "dispatch_failed".into(),
                message: err.to_string(),
            },
        );
        return Err(ControlError::DeviceOffline);
    }

    Ok((StatusCode::ACCEPTED, Json(CreateJobResponse { job_id })))
}

// NOTE: Late-joiner clients — those connecting AFTER the job's terminal
// event (finished/error) has been broadcast and finalize() has removed
// the tracker entry — receive HTTP 404 immediately. The broadcast
// RecvError::Closed path below handles the case where the sender drops
// while a subscriber is already actively streaming. These are distinct
// scenarios. SDK callers must connect to /stream immediately after
// POST /jobs to avoid the late-joiner 404.
async fn stream_job(
    State(state): State<AppState>,
    Extension(claims): Extension<ControlPlaneJwtClaims>,
    Path(job_id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ControlError> {
    if claims.scope != "jobs:execute" {
        return Err(ControlError::Forbidden);
    }
    // Rate-limit BEFORE the job lookup: a valid JWT must not be able
    // to open unlimited concurrent SSE streams (each allocates a
    // broadcast::Receiver + an Arc handle) by hammering this
    // endpoint. Same budget as create_job — per-user token bucket.
    if state
        .control_rate_limiter
        .check_key(&claims.external_user_id)
        .is_err()
    {
        return Err(ControlError::RateLimited);
    }
    let channels = state
        .control_jobs
        .get(&job_id)
        .ok_or(ControlError::JobNotFound)?;
    if channels.external_user_id != claims.external_user_id {
        // 404, not 403: don't leak the existence of another user's
        // job via a status-code oracle.
        return Err(ControlError::JobNotFound);
    }
    // Enforce device_ids allowlist if the token is scoped to specific devices.
    if let Some(allowed) = &claims.device_ids
        && !allowed.contains(&channels.device_id)
    {
        return Err(ControlError::Forbidden);
    }
    let mut rx = channels.subscribe();
    // Release the per-entry Arc so the entry can be dropped when
    // finalize() runs — otherwise we'd keep it alive via this handle
    // and leak the entry even after terminal events.
    drop(channels);

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let rendered = render_sse_event(&event);
                    let is_terminal = matches!(
                        event,
                        ControlJobEvent::Finished { .. } | ControlJobEvent::Error { .. }
                    );
                    yield Ok::<_, Infallible>(rendered);
                    if is_terminal {
                        break;
                    }
                }
                Err(RecvError::Closed) => {
                    // Sender dropped (entry removed without a
                    // terminal event — shouldn't happen in normal
                    // flow, but we close the stream cleanly if it
                    // does).
                    break;
                }
                Err(RecvError::Lagged(_)) => {
                    // Slow subscriber fell behind. Report once and
                    // then close the stream — the SDK is expected to
                    // reconnect / re-fetch rather than miss events
                    // silently.
                    yield Ok(Event::default()
                        .event("error")
                        .data(r#"{"code":"stream_lagged","message":"client fell behind"}"#));
                    break;
                }
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    ))
}

fn render_sse_event(event: &ControlJobEvent) -> Event {
    // `Event::json_data` uses serde_json to serialize the payload,
    // which escapes newlines inside strings. That's the property that
    // lets us deliver a multi-line stdout chunk in a single SSE frame
    // without it being mis-split on a `\n\n` sequence inside the
    // data.
    //
    // The `event:` name is the tag field from our serde
    // representation (`stdout`, `stderr`, `progress`, `finished`,
    // `error`). We serialize manually so the on-wire `data:` is JUST
    // the `data` field — dropping the `event` tag that serde would
    // otherwise include.
    let (name, payload) = match event {
        ControlJobEvent::Stdout { chunk } => ("stdout", serde_json::json!({ "chunk": chunk })),
        ControlJobEvent::Stderr { chunk } => ("stderr", serde_json::json!({ "chunk": chunk })),
        ControlJobEvent::Progress { percent, message } => (
            "progress",
            match message {
                Some(msg) => serde_json::json!({ "percent": percent, "message": msg }),
                None => serde_json::json!({ "percent": percent }),
            },
        ),
        ControlJobEvent::Finished {
            exit_code,
            duration_ms,
        } => (
            "finished",
            serde_json::json!({ "exitCode": exit_code, "durationMs": duration_ms }),
        ),
        ControlJobEvent::Error { code, message } => (
            "error",
            serde_json::json!({ "code": code, "message": message }),
        ),
    };
    // `json_data` can fail only on non-serializable values — our
    // payloads are always plain JSON, so an unwrap is fine here but
    // we fall back to a stringified data line for defense.
    Event::default()
        .event(name)
        .json_data(payload)
        .unwrap_or_else(|_| Event::default().event(name).data(""))
}

async fn cancel_job(
    State(state): State<AppState>,
    Extension(claims): Extension<ControlPlaneJwtClaims>,
    Path(job_id): Path<String>,
) -> Result<StatusCode, ControlError> {
    if claims.scope != "jobs:execute" {
        return Err(ControlError::Forbidden);
    }
    // Rate-limit BEFORE the job lookup: prevents a valid JWT from
    // spamming /cancel unbounded, which would otherwise fan out an
    // unbounded stream of CancelJob envelopes to the daemon.
    if state
        .control_rate_limiter
        .check_key(&claims.external_user_id)
        .is_err()
    {
        return Err(ControlError::RateLimited);
    }
    let channels = state
        .control_jobs
        .get(&job_id)
        .ok_or(ControlError::JobNotFound)?;
    if channels.external_user_id != claims.external_user_id {
        return Err(ControlError::JobNotFound);
    }
    // Enforce device_ids allowlist if the token is scoped to specific devices.
    if let Some(allowed) = &claims.device_ids
        && !allowed.contains(&channels.device_id)
    {
        return Err(ControlError::Forbidden);
    }
    let device_id = channels.device_id.clone();
    drop(channels);
    let envelope = ahand_protocol::Envelope {
        device_id: device_id.clone(),
        msg_id: format!("control-cancel-{job_id}"),
        ts_ms: now_ms(),
        payload: Some(ahand_protocol::envelope::Payload::CancelJob(
            ahand_protocol::CancelJob {
                job_id: job_id.clone(),
            },
        )),
        ..Default::default()
    };
    // Best-effort: even if the daemon isn't online, return 202 so the
    // SDK has a single contract for "we delivered your intent". If
    // the device genuinely can't receive the cancel (offline, WS
    // closed), the daemon's own timeout will eventually terminate
    // the job locally.
    let _ = state.connections.send_envelope(&device_id, envelope).await;
    Ok(StatusCode::ACCEPTED)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod render_tests {
    //! Unit tests for [`render_sse_event`] that cover branches not
    //! exercised by the integration suite (specifically
    //! progress-with-`Some(message)` and the `json_data` fallback).
    use super::render_sse_event;
    use crate::control_jobs::ControlJobEvent;

    fn event_as_string(event: &ControlJobEvent) -> String {
        // axum's `sse::Event` doesn't expose serialized bytes for
        // inspection, so we format the Debug representation which
        // contains the wire name + data.
        format!("{:?}", render_sse_event(event))
    }

    #[test]
    fn progress_with_message_includes_message_field() {
        let ev = ControlJobEvent::Progress {
            percent: 50,
            message: Some("halfway there".into()),
        };
        let rendered = event_as_string(&ev);
        assert!(rendered.contains("halfway there"), "was: {rendered}");
        assert!(rendered.contains("progress"), "was: {rendered}");
    }

    #[test]
    fn progress_without_message_omits_message_field() {
        let ev = ControlJobEvent::Progress {
            percent: 10,
            message: None,
        };
        let rendered = event_as_string(&ev);
        assert!(!rendered.contains("message"), "was: {rendered}");
    }

    #[test]
    fn all_event_variants_render_without_panic() {
        for ev in [
            ControlJobEvent::Stdout { chunk: "x".into() },
            ControlJobEvent::Stderr { chunk: "y".into() },
            ControlJobEvent::Progress {
                percent: 0,
                message: None,
            },
            ControlJobEvent::Finished {
                exit_code: 0,
                duration_ms: 1,
            },
            ControlJobEvent::Error {
                code: "c".into(),
                message: "m".into(),
            },
        ] {
            let _ = render_sse_event(&ev);
        }
    }
}

#[derive(Debug)]
pub enum ControlError {
    Unauthorized,
    Forbidden,
    BadRequest(String),
    DeviceNotFound,
    DeviceOffline,
    JobNotFound,
    RateLimited,
    Internal(String),
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope<'a> {
    error: ErrorBody<'a>,
}

#[derive(Debug, Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: String,
}

impl IntoResponse for ControlError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            ControlError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                "Authentication required".to_string(),
            ),
            ControlError::Forbidden => (
                StatusCode::FORBIDDEN,
                "FORBIDDEN",
                "Control-plane JWT does not grant access to this device".to_string(),
            ),
            ControlError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "VALIDATION_ERROR", msg),
            ControlError::DeviceNotFound => (
                StatusCode::NOT_FOUND,
                "DEVICE_NOT_FOUND",
                "Device not found".to_string(),
            ),
            ControlError::DeviceOffline => (
                StatusCode::NOT_FOUND,
                "DEVICE_OFFLINE",
                "Device is not currently connected".to_string(),
            ),
            ControlError::JobNotFound => (
                StatusCode::NOT_FOUND,
                "JOB_NOT_FOUND",
                "Job not found".to_string(),
            ),
            ControlError::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "RATE_LIMITED",
                "Rate limit exceeded for this user".to_string(),
            ),
            ControlError::Internal(msg) => {
                tracing::warn!(error = %msg, "control-plane internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "INTERNAL_ERROR",
                    "Internal server error".to_string(),
                )
            }
        };
        (
            status,
            Json(ErrorEnvelope {
                error: ErrorBody { code, message },
            }),
        )
            .into_response()
    }
}

// ──────────────────────────────────────────────────────────────────────
// `POST /api/control/browser` — synchronous worker-side browser command.
//
// Wire-format twin of the dashboard `POST /api/browser` handler with
// two differences:
//   1. Auth is via control-plane JWT (mounted under the same middleware
//      that protects `/api/control/jobs`), and ownership is checked
//      against `claims.external_user_id == device.external_user_id`.
//   2. The response includes a `duration_ms` field measuring elapsed
//      time from request start to service return — the SDK surfaces
//      this so callers can observe round-trip latency.
//
// Both endpoints share `browser_service::execute()` (refactored in
// Task 8) and the same error mapper `http::browser::map_service_error`,
// so error codes and HTTP statuses are byte-for-byte identical to the
// dashboard endpoint.
// ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ControlBrowserRequest {
    pub device_id: String,
    pub session_id: String,
    pub action: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    #[serde(default = "default_browser_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub correlation_id: Option<String>,
}

fn default_browser_timeout_ms() -> u64 {
    30_000
}

#[derive(Debug, Serialize)]
pub struct ControlBrowserResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_mime: Option<String>,
    pub duration_ms: u64,
}

pub async fn browser_command_control(
    Extension(claims): Extension<ControlPlaneJwtClaims>,
    State(state): State<AppState>,
    body: Result<Json<ControlBrowserRequest>, JsonRejection>,
) -> ApiResult<Json<ControlBrowserResponse>> {
    // Validate the scope claim before any DB / WS work, mirroring the
    // sibling handlers (`create_job`, `stream_job`, `cancel_job`). A
    // token minted with a non-`jobs:execute` scope (e.g. `jobs:read`)
    // must be rejected before we touch the device store or rate
    // limiter.
    if claims.scope != "jobs:execute" {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "FORBIDDEN",
            "Control-plane JWT scope does not permit this action",
        ));
    }

    let Json(body) = body.map_err(ApiError::from_json_rejection)?;

    // Per-user rate-limit BEFORE any DB / WS work, mirroring the jobs
    // handler. A storm of bogus POSTs from one tenant can't DOS the
    // device store or burn WS dispatch slots for other tenants.
    if state
        .control_rate_limiter
        .check_key(&claims.external_user_id)
        .is_err()
    {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
            "Rate limit exceeded for this user",
        ));
    }

    // Ownership check: the device must exist AND be owned by the
    // calling user. We deliberately return 403 (not 404) on mismatch
    // here because the SDK's own error contract distinguishes
    // "you don't own this device" from "device is offline / unknown" —
    // and at this point the JWT has already authenticated, so the
    // status-code oracle is a non-issue (the caller can prove their
    // own user id from the JWT they minted).
    let device = state
        .devices
        .find_by_id(&body.device_id)
        .await
        .map_err(|e| ApiError::internal(format!("device store: {e}")))?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "DEVICE_NOT_FOUND",
                format!("Device {} not found", body.device_id),
            )
        })?;
    if device.external_user_id.as_deref() != Some(claims.external_user_id.as_str()) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "NOT_DEVICE_OWNER",
            format!("Caller does not own device {}", body.device_id),
        ));
    }
    // Device-allowlist enforcement: if the JWT is scoped to a subset
    // of devices, refuse 403 just like `create_job` does.
    if let Some(allowed) = &claims.device_ids
        && !allowed.contains(&body.device_id)
    {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "FORBIDDEN",
            "Control-plane JWT does not grant access to this device",
        ));
    }

    let started = std::time::Instant::now();

    // Stringify params once; an empty `params` field is forwarded as
    // an empty string (the daemon already accepts that).
    let params_json = body
        .params
        .map(|p| serde_json::to_string(&p).unwrap_or_default())
        .unwrap_or_default();

    let response = browser_service::execute(
        &state,
        BrowserCommandInput {
            device_id: body.device_id,
            session_id: body.session_id,
            action: body.action,
            params_json,
            timeout_ms: body.timeout_ms,
            correlation_id: body.correlation_id,
        },
    )
    .await
    .map_err(map_service_error)?;

    let duration_ms = started.elapsed().as_millis() as u64;

    let data = if response.result_json.is_empty() {
        None
    } else {
        serde_json::from_str(&response.result_json).ok()
    };
    let binary_data = if response.binary_data.is_empty() {
        None
    } else {
        Some(base64::engine::general_purpose::STANDARD.encode(&response.binary_data))
    };
    let binary_mime = if response.binary_mime.is_empty() {
        None
    } else {
        Some(response.binary_mime)
    };
    let error = if response.error.is_empty() {
        None
    } else {
        Some(response.error)
    };

    Ok(Json(ControlBrowserResponse {
        success: response.success,
        data,
        error,
        binary_data,
        binary_mime,
        duration_ms,
    }))
}

// ──────────────────────────────────────────────────────────────────────
// `POST /api/control/files` — synchronous worker-side file operation.
//
// Wire-format twin of the dashboard `POST /api/devices/{device_id}/files`
// handler with three differences:
//   1. Auth is via control-plane JWT (mounted under the same middleware
//      that protects `/api/control/jobs`), and ownership is checked
//      against `claims.external_user_id == device.external_user_id`.
//   2. The body is JSON (matching the browser endpoint's symmetry
//      decision), not raw protobuf — see `http::control_files_dto` for
//      the schema.
//   3. The response includes a `duration_ms` field measuring elapsed
//      time from request start to service return — same as
//      `/api/control/browser`, so SDK callers can observe round-trip
//      latency.
//
// Both endpoints share `file_service::execute()` and the same hub-error
// mapper `http::files::map_service_error`, so hub-level error codes
// and HTTP statuses match the dashboard endpoint byte-for-byte.
// Daemon-side errors (e.g. NOT_FOUND, POLICY_DENIED) come back as HTTP
// 200 with `success: false` plus an `error.code` field — the dashboard
// returns the same information inside its protobuf envelope, just in a
// different format.
// ──────────────────────────────────────────────────────────────────────

use crate::file_service::{self, FileServiceError};
use crate::http::control_files_dto::{self, ControlFilesRequest, ControlFilesResponse, DtoError};
use crate::http::files::map_service_error as map_file_service_error;

/// Default file-op timeout when the SDK doesn't pass one. Matches the
/// browser endpoint default and is bounded by `state.file_request_timeout`
/// (the hub-side hard ceiling) — the smaller of the two wins.
const DEFAULT_FILE_TIMEOUT_MS: u64 = 30_000;

pub async fn control_files(
    Extension(claims): Extension<ControlPlaneJwtClaims>,
    State(state): State<AppState>,
    body: Result<Json<ControlFilesRequest>, JsonRejection>,
) -> ApiResult<Json<ControlFilesResponse>> {
    if claims.scope != "jobs:execute" {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "FORBIDDEN",
            "Control-plane JWT scope does not permit this action",
        ));
    }

    let Json(body) = body.map_err(ApiError::from_json_rejection)?;

    // Per-user rate-limit BEFORE any DB / WS work, mirroring the jobs
    // and browser handlers. A storm of bogus POSTs from one tenant
    // can't DOS the device store or burn WS dispatch slots for other
    // tenants.
    if state
        .control_rate_limiter
        .check_key(&claims.external_user_id)
        .is_err()
    {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
            "Rate limit exceeded for this user",
        ));
    }

    if body.device_id.is_empty() {
        return Err(ApiError::validation("device_id must not be empty"));
    }
    if body.operation.is_empty() {
        return Err(ApiError::validation("operation must not be empty"));
    }

    // Ownership check: the device must exist AND be owned by the
    // calling user. Mirrors `browser_command_control` — 403 (not 404)
    // on mismatch, since the JWT has already authenticated and the
    // caller can prove their own user id from the JWT they minted.
    let device = state
        .devices
        .find_by_id(&body.device_id)
        .await
        .map_err(|e| ApiError::internal(format!("device store: {e}")))?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "DEVICE_NOT_FOUND",
                format!("Device {} not found", body.device_id),
            )
        })?;
    if device.external_user_id.as_deref() != Some(claims.external_user_id.as_str()) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "NOT_DEVICE_OWNER",
            format!("Caller does not own device {}", body.device_id),
        ));
    }
    if let Some(allowed) = &claims.device_ids
        && !allowed.contains(&body.device_id)
    {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "FORBIDDEN",
            "Control-plane JWT does not grant access to this device",
        ));
    }

    // Build the protobuf request. JSON parsing errors (unknown op,
    // missing required field, malformed param) map to 400.
    let operation = control_files_dto::build_request_operation(&body.operation, body.params)
        .map_err(|e| match e {
            DtoError::UnknownOperation(op) => ApiError::new(
                StatusCode::BAD_REQUEST,
                "UNKNOWN_OPERATION",
                format!(
                    "operation '{op}' is not one of: stat, list, glob, read_text, \
                         read_binary, read_image, write, edit, delete, chmod, mkdir, \
                         copy, move, create_symlink"
                ),
            ),
            DtoError::InvalidParams { op, message } => ApiError::new(
                StatusCode::BAD_REQUEST,
                "INVALID_PARAMS",
                format!("invalid params for operation '{op}': {message}"),
            ),
        })?;

    // Always mint a fresh request_id at the hub level — the wire-level
    // `correlation_id` is a logical-retry hint, surfaced separately
    // for future hub-side dedupe (parity with the browser endpoint).
    let request_id = uuid::Uuid::new_v4().to_string();
    let mut proto_request = ahand_protocol::FileRequest {
        request_id: request_id.clone(),
        operation: Some(operation),
    };

    // Same S3 upload-URL injection the dashboard handler does. Kept at
    // the caller (not inside file_service::execute) because the
    // failure modes — 503 S3_DISABLED, 400 INVALID_S3_OBJECT_KEY —
    // shouldn't collapse into 500 Internal.
    crate::http::files::maybe_inject_full_write_download_url(&state, &mut proto_request).await?;

    // Caller-supplied timeout caps the hub's default but cannot exceed
    // `state.file_request_timeout` (the operator-configured hub-wide
    // ceiling, plumbed from `Config::file_request_timeout_ms`).
    let requested_ms = body
        .timeout_ms
        .unwrap_or(DEFAULT_FILE_TIMEOUT_MS)
        .max(1_000);
    let ceiling_ms = state
        .file_request_timeout
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;
    let effective_ms = requested_ms.min(ceiling_ms.max(1_000));
    let timeout = std::time::Duration::from_millis(effective_ms);

    let started = std::time::Instant::now();
    let response = file_service::execute(&state, &device.id, proto_request, timeout)
        .await
        .map_err(|err| match err {
            // Surface the same wire-level errors the dashboard does so
            // the SDK gets a single contract to teach.
            FileServiceError::AtCapacity
            | FileServiceError::Duplicate { .. }
            | FileServiceError::DeviceOffline { .. }
            | FileServiceError::Timeout { .. }
            | FileServiceError::ChannelClosed
            | FileServiceError::Internal(_) => map_file_service_error(err),
        })?;
    let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

    // Elevate daemon-side `policy_denied` to a hub-level HTTP 403 with
    // code `POLICY_DENIED`. Spec mandate: operationally, a refused
    // path is a different category than a missing file or an I/O
    // error — the caller can't fix it by retrying / falling back, so
    // surface it at the HTTP layer where typed-error consumers (the
    // SDK's `policy_denied` `CloudClientErrorCode`) can branch on it
    // without having to inspect the JSON envelope. Other daemon-level
    // errors (`not_found`, `io`, `encoding`, ...) stay inside the
    // success:false body — those ARE typically things callers want
    // to handle gracefully.
    if let Some(ahand_protocol::file_response::Result::Error(file_err)) = &response.result
        && file_err.code == ahand_protocol::FileErrorCode::PolicyDenied as i32
    {
        let message = if file_err.message.is_empty() {
            format!(
                "Daemon refused operation by policy on path {}",
                file_err.path
            )
        } else {
            file_err.message.clone()
        };
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "POLICY_DENIED",
            message,
        ));
    }

    let envelope =
        control_files_dto::build_response_envelope(response, &body.operation, duration_ms);
    Ok(Json(envelope))
}
