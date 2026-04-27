# Hub Browser Tab Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add browser automation support to the hub — a `POST /api/browser` endpoint in Rust and a "Browser" tab in hub-dashboard — so devices with the `"browser"` capability can receive browser commands through the hub.

**Architecture:** The hub receives browser requests via HTTP, forwards them as protobuf `BrowserRequest` envelopes over the device WebSocket, awaits `BrowserResponse` via oneshot channel, and returns JSON (with base64-encoded binary data). The frontend adds a `DeviceBrowser` React component (ported from the old SolidJS `BrowserPanel`) behind a capability gate.

**Tech Stack:** Rust/Axum (hub backend), ahand-protocol protobuf, Next.js/React (hub-dashboard), DashMap + tokio oneshot (pending request tracking)

**Spec:** `docs/superpowers/specs/2026-04-12-hub-browser-tab-design.md`

---

## File Structure

### Hub Backend (Rust)
| File | Action | Responsibility |
|------|--------|---------------|
| `crates/ahand-hub/src/http/browser.rs` | Create | HTTP handler for `POST /api/browser` + request/response types |
| `crates/ahand-hub/src/http/mod.rs` | Modify | Add `pub mod browser` and wire route |
| `crates/ahand-hub/src/state.rs` | Modify | Add `BrowserPendingMap` to `AppState` |
| `crates/ahand-hub/src/ws/device_gateway.rs` | Modify | Route inbound `BrowserResponse` to pending map |
| `crates/ahand-hub/tests/support/mod.rs` | Modify | Add browser test helpers to `TestDevice` |
| `crates/ahand-hub/tests/browser_api.rs` | Create | Integration tests for browser endpoint |

### Frontend (hub-dashboard)
| File | Action | Responsibility |
|------|--------|---------------|
| `apps/hub-dashboard/src/components/device-browser.tsx` | Create | Browser panel component (React port) |
| `apps/hub-dashboard/src/components/device-tabs.tsx` | Modify | Add "Browser" tab with capability gate |
| `apps/hub-dashboard/src/app/(dashboard)/devices/[id]/page.tsx` | Modify | Pass `capabilities` to `DeviceTabs` |
| `apps/hub-dashboard/src/app/globals.css` | Modify | Add browser panel styles |

---

### Task 1: Add BrowserPendingMap to AppState and route BrowserResponse in device gateway

**Goal:** Enable the hub to track pending browser requests and match inbound `BrowserResponse` envelopes to waiting HTTP handlers.

**Files:**
- Modify: `crates/ahand-hub/src/state.rs:22-45` (add field to `AppState`)
- Modify: `crates/ahand-hub/src/ws/device_gateway.rs:540-543` (route `BrowserResponse` from device frames)

**Acceptance Criteria:**
- [ ] `AppState` has a `browser_pending: Arc<DashMap<String, tokio::sync::oneshot::Sender<ahand_protocol::BrowserResponse>>>` field
- [ ] Inbound `BrowserResponse` envelopes from devices are matched by `request_id` and forwarded via oneshot
- [ ] Unmatched `BrowserResponse` envelopes are logged and discarded (no panic)
- [ ] Existing job frame handling is unaffected

**Verify:** `cargo test -p ahand-hub` → all existing tests pass

**Steps:**

- [ ] **Step 1: Add `browser_pending` field to `AppState`**

In `crates/ahand-hub/src/state.rs`, add a new field after `connections`:

```rust
// In the AppState struct definition, after `connections`:
pub browser_pending: Arc<DashMap<String, tokio::sync::oneshot::Sender<ahand_protocol::BrowserResponse>>>,
```

In `AppState::from_config`, initialize it:

```rust
let browser_pending = Arc::new(DashMap::new());
```

And include it in the `Self { ... }` construction.

- [ ] **Step 2: Route `BrowserResponse` in device frame handling**

The device gateway currently passes all binary frames to `state.jobs.handle_device_frame()`. We need to intercept `BrowserResponse` payloads before they reach the job runtime.

In `crates/ahand-hub/src/ws/device_gateway.rs`, change the `WsMessage::Binary` handler at line 541-543 from:

```rust
WsMessage::Binary(frame) => {
    *last_inbound_at.lock().await = tokio::time::Instant::now();
    state.jobs.handle_device_frame(&device_id, &frame).await?;
}
```

to:

```rust
WsMessage::Binary(frame) => {
    *last_inbound_at.lock().await = tokio::time::Instant::now();
    let envelope = ahand_protocol::Envelope::decode(frame.as_ref())?;
    if let Some(ahand_protocol::envelope::Payload::BrowserResponse(ref browser_resp)) = envelope.payload {
        if let Some((_, sender)) = state.browser_pending.remove(&browser_resp.request_id) {
            let _ = sender.send(browser_resp.clone());
        } else {
            tracing::warn!(
                request_id = %browser_resp.request_id,
                "received BrowserResponse with no pending request"
            );
        }
        state.connections.observe_inbound(&device_id, envelope.seq, envelope.ack)?;
    } else {
        state.jobs.handle_device_frame(&device_id, &frame).await?;
    }
}
```

Note: `prost::Message` is already imported in `device_gateway.rs`.

- [ ] **Step 3: Run existing tests to verify no regressions**

Run: `cargo test -p ahand-hub`
Expected: All tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/ahand-hub/src/state.rs crates/ahand-hub/src/ws/device_gateway.rs
git commit -m "feat(hub): add BrowserPendingMap to AppState and route BrowserResponse in gateway"
```

---

### Task 2: Add POST /api/browser HTTP handler

**Goal:** Create the browser HTTP endpoint that receives browser commands, sends them to devices via WebSocket, and awaits responses.

**Files:**
- Create: `crates/ahand-hub/src/http/browser.rs`
- Modify: `crates/ahand-hub/src/http/mod.rs:1-44`
- Modify: `crates/ahand-hub/Cargo.toml`

**Acceptance Criteria:**
- [ ] `POST /api/browser` accepts `{device_id, session_id, action, params?, timeout_ms?}` with dashboard auth
- [ ] Handler builds a `BrowserRequest` protobuf, sends via `ConnectionRegistry`, awaits oneshot with timeout
- [ ] Response returns `{success, data, error, binary_data, binary_mime}` with base64-encoded binary
- [ ] Returns 404 if device not connected, 400 if device lacks `"browser"` capability, 504 on timeout

**Verify:** `cargo test -p ahand-hub` → all tests pass

**Steps:**

- [ ] **Step 1: Add `base64` dependency**

In `crates/ahand-hub/Cargo.toml`, add under `[dependencies]`:

```toml
base64 = "0.22"
```

- [ ] **Step 2: Create `crates/ahand-hub/src/http/browser.rs`**

```rust
use axum::extract::rejection::JsonRejection;
use axum::extract::{Json, State};
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::auth::AuthContextExt;
use crate::http::api_error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct BrowserCommandRequest {
    pub device_id: String,
    pub session_id: String,
    pub action: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_timeout_ms() -> u64 {
    30_000
}

#[derive(Serialize)]
pub struct BrowserCommandResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_mime: Option<String>,
}

pub async fn browser_command(
    auth: AuthContextExt,
    State(state): State<AppState>,
    body: Result<Json<BrowserCommandRequest>, JsonRejection>,
) -> ApiResult<Json<BrowserCommandResponse>> {
    auth.require_dashboard_access()?;
    let Json(body) = body.map_err(ApiError::from_json_rejection)?;

    let device = state
        .devices
        .get(&body.device_id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::new(
                axum::http::StatusCode::NOT_FOUND,
                "DEVICE_NOT_FOUND",
                format!("Device {} not found", body.device_id),
            )
        })?;

    if !device.online {
        return Err(ApiError::new(
            axum::http::StatusCode::NOT_FOUND,
            "DEVICE_OFFLINE",
            "Device is not connected",
        ));
    }

    if !device.capabilities.iter().any(|c| c == "browser") {
        return Err(ApiError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "NO_BROWSER_CAPABILITY",
            "Device does not support browser",
        ));
    }

    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.browser_pending.insert(request_id.clone(), tx);

    let params_json = body
        .params
        .map(|p| serde_json::to_string(&p).unwrap_or_default())
        .unwrap_or_default();

    let envelope = ahand_protocol::Envelope {
        device_id: body.device_id.clone(),
        msg_id: format!("browser-{request_id}"),
        ts_ms: now_ms(),
        payload: Some(ahand_protocol::envelope::Payload::BrowserRequest(
            ahand_protocol::BrowserRequest {
                request_id: request_id.clone(),
                session_id: body.session_id,
                action: body.action,
                params_json,
                timeout_ms: body.timeout_ms,
            },
        )),
        ..Default::default()
    };

    if let Err(err) = state.connections.send(&body.device_id, envelope).await {
        state.browser_pending.remove(&request_id);
        return Err(ApiError::new(
            axum::http::StatusCode::NOT_FOUND,
            "DEVICE_OFFLINE",
            format!("Failed to send to device: {err}"),
        ));
    }

    let timeout = std::time::Duration::from_millis(body.timeout_ms.max(1000));
    let response = match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(_)) => {
            state.browser_pending.remove(&request_id);
            return Err(ApiError::new(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "Browser request channel closed unexpectedly",
            ));
        }
        Err(_) => {
            state.browser_pending.remove(&request_id);
            return Err(ApiError::new(
                axum::http::StatusCode::GATEWAY_TIMEOUT,
                "TIMEOUT",
                "Browser command timed out",
            ));
        }
    };

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

    Ok(Json(BrowserCommandResponse {
        success: response.success,
        data,
        error,
        binary_data,
        binary_mime,
    }))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
```

- [ ] **Step 3: Wire the route in `crates/ahand-hub/src/http/mod.rs`**

Add `pub mod browser;` to the module declarations. Add the route:

```rust
.route("/api/browser", post(browser::browser_command))
```

- [ ] **Step 4: Verify compilation and existing tests**

Run: `cargo test -p ahand-hub`
Expected: All tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/ahand-hub/src/http/browser.rs crates/ahand-hub/src/http/mod.rs crates/ahand-hub/Cargo.toml
git commit -m "feat(hub): add POST /api/browser endpoint"
```

---

### Task 3: Integration tests for browser API

**Goal:** Test the full browser request-response flow through hub, including error cases.

**Files:**
- Modify: `crates/ahand-hub/tests/support/mod.rs` (add browser test helpers)
- Create: `crates/ahand-hub/tests/browser_api.rs`
- Modify: `crates/ahand-hub/Cargo.toml` (add `base64` to dev-dependencies)

**Acceptance Criteria:**
- [ ] Test: successful browser command roundtrip (send request → device responds → hub returns result)
- [ ] Test: browser command with binary data (screenshot) returns base64-encoded data
- [ ] Test: request to offline device returns 404
- [ ] Test: request to device without browser capability returns 400
- [ ] Test: request that times out returns 504
- [ ] Test: unauthenticated request returns 401

**Verify:** `cargo test -p ahand-hub browser` → all 6 tests pass

**Steps:**

- [ ] **Step 1: Add browser helpers to `TestDevice` in `crates/ahand-hub/tests/support/mod.rs`**

Add `BrowserRequest` and `BrowserResponse` to the imports:

```rust
use ahand_protocol::{
    BootstrapAuth, BrowserRequest, BrowserResponse, CancelJob, Ed25519Auth, Envelope, Hello,
    HelloAccepted, HelloChallenge, JobEvent, JobFinished, JobRequest, envelope, hello, job_event,
};
```

Add methods to `TestDevice`:

```rust
pub async fn recv_browser_request(&mut self) -> BrowserRequest {
    while let Some(message) = self.socket.next().await {
        let message = message.unwrap();
        match message {
            tokio_tungstenite::tungstenite::Message::Binary(data) => {
                let envelope = Envelope::decode(data.as_ref()).unwrap();
                if let Some(envelope::Payload::BrowserRequest(req)) = envelope.payload {
                    return req;
                }
            }
            _ => {}
        }
    }
    panic!("device socket closed before a browser request arrived");
}

pub async fn send_browser_response(&mut self, response: BrowserResponse) {
    let envelope = Envelope {
        device_id: self.device_id.clone(),
        msg_id: format!("browser-resp-{}", response.request_id),
        ts_ms: now_ms(),
        payload: Some(envelope::Payload::BrowserResponse(response)),
        ..Default::default()
    };
    self.socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            envelope.encode_to_vec().into(),
        ))
        .await
        .unwrap();
}
```

Add `test_state_with_browser_device`:

```rust
pub async fn test_state_with_browser_device() -> AppState {
    let state = AppState::from_config(test_config())
        .await
        .expect("test state should build");
    state
        .devices
        .insert(NewDevice {
            id: "device-1".into(),
            public_key: Some(
                SigningKey::from_bytes(&[7u8; 32])
                    .verifying_key()
                    .to_bytes()
                    .to_vec(),
            ),
            hostname: "seeded-device".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into(), "browser".into()],
            version: Some("0.1.2".into()),
            auth_method: "ed25519".into(),
        })
        .await
        .unwrap();
    state.devices.mark_offline("device-1").await.unwrap();
    state
}
```

Add `signed_hello_with_browser` function:

```rust
pub fn signed_hello_with_browser(device_id: &str, challenge_nonce: &[u8]) -> Envelope {
    let signed_at_ms = next_test_signed_at_ms();
    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    let mut hello = Hello {
        version: "0.1.2".into(),
        hostname: "devbox".into(),
        os: "linux".into(),
        capabilities: vec!["exec".into(), "browser".into()],
        last_ack: 0,
        auth: None,
    };
    let signature = signing_key
        .sign(&ahand_protocol::build_hello_auth_payload(
            device_id, &hello, signed_at_ms, challenge_nonce,
        ))
        .to_bytes()
        .to_vec();
    hello.auth = Some(hello::Auth::Ed25519(Ed25519Auth {
        public_key: signing_key.verifying_key().to_bytes().to_vec(),
        signature,
        signed_at_ms,
    }));
    Envelope {
        device_id: device_id.into(),
        msg_id: "hello-browser-1".into(),
        ts_ms: signed_at_ms,
        payload: Some(envelope::Payload::Hello(hello)),
        ..Default::default()
    }
}
```

Add `attach_browser_device` to `TestServer`:

```rust
pub async fn attach_browser_device(&self, device_id: &str) -> TestDevice {
    let (mut socket, _) = tokio_tungstenite::connect_async(self.ws_url("/ws"))
        .await
        .unwrap();
    let challenge = read_hello_challenge(&mut socket).await;
    let hello = signed_hello_with_browser(device_id, &challenge.nonce);
    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            hello.encode_to_vec().into(),
        ))
        .await
        .unwrap();
    let _ = read_hello_accepted(&mut socket).await;
    TestDevice {
        device_id: device_id.into(),
        socket,
    }
}
```

- [ ] **Step 2: Add `base64` to dev-dependencies**

In `crates/ahand-hub/Cargo.toml`, under `[dev-dependencies]`:

```toml
base64 = "0.22"
```

- [ ] **Step 3: Create `crates/ahand-hub/tests/browser_api.rs`**

```rust
mod support;

use ahand_protocol::BrowserResponse;
use serde_json::Value;

use support::spawn_server_with_state;

#[tokio::test]
async fn browser_command_roundtrip() {
    let state = support::test_state_with_browser_device().await;
    let server = spawn_server_with_state(state).await;
    let token = login_token(&server).await;
    let mut device = server.attach_browser_device("device-1").await;

    let api_task = tokio::spawn({
        let base = server.http_base_url().to_string();
        let token = token.clone();
        async move {
            reqwest::Client::new()
                .post(format!("{base}/api/browser"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "device_id": "device-1",
                    "session_id": "test-session",
                    "action": "snapshot",
                }))
                .send()
                .await
                .unwrap()
        }
    });

    let req = device.recv_browser_request().await;
    assert_eq!(req.action, "snapshot");
    assert_eq!(req.session_id, "test-session");

    device
        .send_browser_response(BrowserResponse {
            request_id: req.request_id,
            session_id: "test-session".into(),
            success: true,
            result_json: r#"{"title":"Example"}"#.into(),
            ..Default::default()
        })
        .await;

    let response = api_task.await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["success"], true);
    assert_eq!(body["data"]["title"], "Example");

    server.shutdown().await;
}

#[tokio::test]
async fn browser_command_with_binary_data() {
    let state = support::test_state_with_browser_device().await;
    let server = spawn_server_with_state(state).await;
    let token = login_token(&server).await;
    let mut device = server.attach_browser_device("device-1").await;

    let api_task = tokio::spawn({
        let base = server.http_base_url().to_string();
        let token = token.clone();
        async move {
            reqwest::Client::new()
                .post(format!("{base}/api/browser"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "device_id": "device-1",
                    "session_id": "test-session",
                    "action": "screenshot",
                }))
                .send()
                .await
                .unwrap()
        }
    });

    let req = device.recv_browser_request().await;
    assert_eq!(req.action, "screenshot");

    let fake_png = vec![0x89, 0x50, 0x4E, 0x47];
    device
        .send_browser_response(BrowserResponse {
            request_id: req.request_id,
            session_id: "test-session".into(),
            success: true,
            result_json: "{}".into(),
            binary_data: fake_png.clone(),
            binary_mime: "image/png".into(),
            ..Default::default()
        })
        .await;

    let response = api_task.await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["success"], true);
    assert_eq!(body["binary_mime"], "image/png");
    let decoded = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        body["binary_data"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(decoded, fake_png);

    server.shutdown().await;
}

#[tokio::test]
async fn browser_command_offline_device_returns_404() {
    let state = support::test_state_with_browser_device().await;
    let server = spawn_server_with_state(state).await;
    let token = login_token(&server).await;

    let response = reqwest::Client::new()
        .post(format!("{}/api/browser", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "device-1",
            "session_id": "test-session",
            "action": "snapshot",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    server.shutdown().await;
}

#[tokio::test]
async fn browser_command_no_capability_returns_400() {
    let state = support::test_state().await;
    let server = spawn_server_with_state(state).await;
    let token = login_token(&server).await;
    let _device = server.attach_test_device("device-1").await;

    let response = reqwest::Client::new()
        .post(format!("{}/api/browser", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "device-1",
            "session_id": "test-session",
            "action": "snapshot",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    server.shutdown().await;
}

#[tokio::test]
async fn browser_command_timeout_returns_504() {
    let state = support::test_state_with_browser_device().await;
    let server = spawn_server_with_state(state).await;
    let token = login_token(&server).await;
    let _device = server.attach_browser_device("device-1").await;

    let response = reqwest::Client::new()
        .post(format!("{}/api/browser", server.http_base_url()))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "device_id": "device-1",
            "session_id": "test-session",
            "action": "snapshot",
            "timeout_ms": 500,
        }))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::GATEWAY_TIMEOUT);
    server.shutdown().await;
}

#[tokio::test]
async fn browser_command_unauthenticated_returns_401() {
    let state = support::test_state_with_browser_device().await;
    let server = spawn_server_with_state(state).await;

    let response = reqwest::Client::new()
        .post(format!("{}/api/browser", server.http_base_url()))
        .json(&serde_json::json!({
            "device_id": "device-1",
            "session_id": "test-session",
            "action": "snapshot",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    server.shutdown().await;
}

async fn login_token(server: &support::TestServer) -> String {
    let body = server
        .post_json(
            "/api/auth/login",
            "",
            serde_json::json!({ "password": "shared-secret" }),
        )
        .await;
    body["token"].as_str().unwrap().to_string()
}
```

- [ ] **Step 4: Run browser tests**

Run: `cargo test -p ahand-hub browser`
Expected: All 6 tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/ahand-hub/tests/browser_api.rs crates/ahand-hub/tests/support/mod.rs crates/ahand-hub/Cargo.toml
git commit -m "test(hub): add integration tests for browser API"
```

---

### Task 4: DeviceBrowser React component

**Goal:** Create the `DeviceBrowser` component by porting the old SolidJS `BrowserPanel` to React, calling the hub's `/api/browser` endpoint.

**Files:**
- Create: `apps/hub-dashboard/src/components/device-browser.tsx`

**Acceptance Criteria:**
- [ ] Component renders session config (session ID input)
- [ ] Actions section: URL + Open, Selector, Value + Fill, button row (Snapshot, Click, Fill, Screenshot, Download, PDF, Close)
- [ ] Collapsible Custom Command section with action name + params JSON inputs
- [ ] Response log: reverse-chronological, with inline image previews, download links, and formatted JSON
- [ ] All commands call `POST` to `buildProxyUrl("/api/browser")` with the device ID
- [ ] Loading state shown while command is executing
- [ ] Clear log button

**Verify:** `cd apps/hub-dashboard && npx next build` → build succeeds

**Steps:**

- [ ] **Step 1: Create `apps/hub-dashboard/src/components/device-browser.tsx`**

```tsx
"use client";

import { useCallback, useState } from "react";
import { buildProxyUrl } from "@/lib/hub-paths";

interface BrowserLogEntry {
  id: number;
  action: string;
  params?: Record<string, unknown>;
  success?: boolean;
  data?: unknown;
  error?: string;
  binaryData?: string;
  binaryMime?: string;
  loading: boolean;
  ts: number;
}

let nextEntryId = 0;

export function DeviceBrowser({ deviceId }: { deviceId: string }) {
  const [sessionId, setSessionId] = useState("test-session");
  const [url, setUrl] = useState("https://example.com");
  const [selector, setSelector] = useState("");
  const [fillValue, setFillValue] = useState("");
  const [customAction, setCustomAction] = useState("");
  const [customParams, setCustomParams] = useState("{}");
  const [log, setLog] = useState<BrowserLogEntry[]>([]);
  const [showCustom, setShowCustom] = useState(false);

  const addEntry = useCallback(
    (action: string, params?: Record<string, unknown>): number => {
      const id = nextEntryId++;
      setLog((prev) => [
        { id, action, params, loading: true, ts: Date.now() },
        ...prev,
      ]);
      return id;
    },
    [],
  );

  const updateEntry = useCallback(
    (
      id: number,
      result: {
        success?: boolean;
        data?: unknown;
        error?: string;
        binary_data?: string;
        binary_mime?: string;
      },
    ) => {
      setLog((prev) =>
        prev.map((e) =>
          e.id === id
            ? {
                ...e,
                success: result.success,
                data: result.data,
                error: result.error,
                binaryData: result.binary_data,
                binaryMime: result.binary_mime,
                loading: false,
              }
            : e,
        ),
      );
    },
    [],
  );

  const send = useCallback(
    async (action: string, params?: Record<string, unknown>) => {
      const entryId = addEntry(action, params);
      try {
        const res = await fetch(buildProxyUrl("/api/browser"), {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            device_id: deviceId,
            session_id: sessionId,
            action,
            params,
          }),
        });
        const data = await res.json();
        updateEntry(entryId, data);
      } catch (e) {
        updateEntry(entryId, {
          success: false,
          error: e instanceof Error ? e.message : String(e),
        });
      }
    },
    [deviceId, sessionId, addEntry, updateEntry],
  );

  const handleOpen = () => {
    if (!url.trim()) return;
    send("open", { url: url.trim() });
  };
  const handleSnapshot = () => send("snapshot");
  const handleClick = () => {
    if (!selector.trim()) return;
    send("click", { selector: selector.trim() });
  };
  const handleFill = () => {
    if (!selector.trim()) return;
    send("fill", { selector: selector.trim(), value: fillValue });
  };
  const handleScreenshot = () => send("screenshot");
  const handleDownload = () => {
    if (!selector.trim()) return;
    send("download", { selector: selector.trim() });
  };
  const handlePdf = () => send("pdf");
  const handleClose = () => send("close");

  const handleCustom = () => {
    if (!customAction.trim()) return;
    try {
      const params = JSON.parse(customParams);
      send(customAction.trim(), params);
    } catch {
      const entryId = addEntry(customAction.trim());
      updateEntry(entryId, { success: false, error: "Invalid JSON in params" });
    }
  };

  const makeBlobUrl = (base64: string, mime: string): string => {
    const raw = atob(base64);
    const bytes = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
    return URL.createObjectURL(new Blob([bytes], { type: mime }));
  };

  const extractFilename = (entry: BrowserLogEntry): string => {
    const path = (entry.data as Record<string, unknown>)?.path;
    if (typeof path === "string") {
      const parts = path.split("/");
      return parts[parts.length - 1] || `${entry.action}-result`;
    }
    return `${entry.action}-result`;
  };

  return (
    <div className="browser-panel">
      <div className="browser-section">
        <div className="browser-section-title">Session</div>
        <div className="browser-form-row">
          <label className="browser-label">Session ID</label>
          <input
            className="browser-input"
            value={sessionId}
            onChange={(e) => setSessionId(e.target.value)}
            placeholder="browser session identifier"
          />
        </div>
      </div>

      <div className="browser-section">
        <div className="browser-section-title">Actions</div>
        <div className="browser-form-row">
          <label className="browser-label">URL</label>
          <input
            className="browser-input"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            placeholder="https://example.com"
            onKeyDown={(e) => e.key === "Enter" && handleOpen()}
          />
          <button className="browser-btn browser-btn-primary" onClick={handleOpen}>
            Open
          </button>
        </div>
        <div className="browser-form-row">
          <label className="browser-label">Selector</label>
          <input
            className="browser-input"
            value={selector}
            onChange={(e) => setSelector(e.target.value)}
            placeholder="@e2 or CSS selector"
          />
        </div>
        <div className="browser-form-row">
          <label className="browser-label">Value</label>
          <input
            className="browser-input"
            value={fillValue}
            onChange={(e) => setFillValue(e.target.value)}
            placeholder="text for fill action"
            onKeyDown={(e) => e.key === "Enter" && handleFill()}
          />
        </div>
        <div className="browser-actions-row">
          <button className="browser-btn" onClick={handleSnapshot}>Snapshot</button>
          <button className="browser-btn" onClick={handleClick} disabled={!selector.trim()}>Click</button>
          <button className="browser-btn" onClick={handleFill} disabled={!selector.trim()}>Fill</button>
          <button className="browser-btn" onClick={handleScreenshot}>Screenshot</button>
          <button className="browser-btn" onClick={handleDownload} disabled={!selector.trim()}>Download</button>
          <button className="browser-btn" onClick={handlePdf}>PDF</button>
          <button className="browser-btn browser-btn-danger" onClick={handleClose}>Close</button>
        </div>
      </div>

      <div className="browser-section">
        <div className="browser-section-header">
          <span className="browser-section-title">Custom Command</span>
          <button className="browser-btn browser-btn-sm" onClick={() => setShowCustom(!showCustom)}>
            {showCustom ? "Hide" : "Show"}
          </button>
        </div>
        {showCustom && (
          <>
            <div className="browser-form-row">
              <label className="browser-label">Action</label>
              <input
                className="browser-input"
                value={customAction}
                onChange={(e) => setCustomAction(e.target.value)}
                placeholder="e.g. hover, select, drag"
              />
            </div>
            <div className="browser-form-row">
              <label className="browser-label">Params</label>
              <input
                className="browser-input"
                value={customParams}
                onChange={(e) => setCustomParams(e.target.value)}
                placeholder='{"key": "value"}'
                onKeyDown={(e) => e.key === "Enter" && handleCustom()}
              />
            </div>
            <button className="browser-btn browser-btn-primary" onClick={handleCustom}>
              Send
            </button>
          </>
        )}
      </div>

      <div className="browser-section">
        <div className="browser-section-header">
          <span className="browser-section-title">
            Response Log ({log.length})
          </span>
          {log.length > 0 && (
            <button className="browser-btn browser-btn-sm" onClick={() => setLog([])}>
              Clear
            </button>
          )}
        </div>
        {log.length === 0 ? (
          <p className="empty-state">No browser commands sent yet.</p>
        ) : (
          <div className="browser-log">
            {log.map((entry) => (
              <div className="browser-log-entry" key={entry.id}>
                <div className="browser-log-header">
                  <span className="browser-log-action">{entry.action}</span>
                  {entry.params && (
                    <span className="browser-log-params">
                      {JSON.stringify(entry.params)}
                    </span>
                  )}
                  <span className="browser-log-time">
                    {new Date(entry.ts).toLocaleTimeString()}
                  </span>
                </div>
                <div className="browser-log-body">
                  {entry.loading ? (
                    <span className="browser-log-loading">Loading...</span>
                  ) : (
                    <>
                      {entry.success !== undefined && (
                        <span className={entry.success ? "browser-log-success" : "browser-log-fail"}>
                          {entry.success ? "SUCCESS" : "FAILED"}
                        </span>
                      )}
                      {entry.error && (
                        <span className="browser-log-fail"> {entry.error}</span>
                      )}
                      {entry.binaryData && entry.binaryMime && (
                        <div className="browser-log-binary">
                          {entry.binaryMime.startsWith("image/") ? (
                            <img
                              className="browser-preview-img"
                              src={`data:${entry.binaryMime};base64,${entry.binaryData}`}
                              alt={extractFilename(entry)}
                            />
                          ) : (
                            <a
                              className="browser-download-link"
                              href={makeBlobUrl(entry.binaryData, entry.binaryMime)}
                              download={extractFilename(entry)}
                            >
                              Download {extractFilename(entry)} ({entry.binaryMime})
                            </a>
                          )}
                        </div>
                      )}
                      {entry.data !== undefined && entry.data !== null && (
                        <pre className="browser-log-data">
                          {typeof entry.data === "string"
                            ? entry.data
                            : JSON.stringify(entry.data, null, 2)}
                        </pre>
                      )}
                    </>
                  )}
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Verify build**

Run: `cd apps/hub-dashboard && npx next build`
Expected: Build succeeds

- [ ] **Step 3: Commit**

```bash
git add apps/hub-dashboard/src/components/device-browser.tsx
git commit -m "feat(dashboard): add DeviceBrowser component"
```

---

### Task 5: Integrate Browser tab into DeviceTabs and add styles

**Goal:** Wire the Browser tab into the device detail page, gated on capability, and add CSS styles.

**Files:**
- Modify: `apps/hub-dashboard/src/app/(dashboard)/devices/[id]/page.tsx:73`
- Modify: `apps/hub-dashboard/src/components/device-tabs.tsx`
- Modify: `apps/hub-dashboard/src/app/globals.css`

**Acceptance Criteria:**
- [ ] Browser tab appears only when device is online AND has `"browser"` capability
- [ ] `capabilities` array is passed from device detail page to `DeviceTabs`
- [ ] Tab defaults: if online + browser → "browser", else if online → "terminal", else → "jobs"
- [ ] Browser panel styles match the dark theme

**Verify:** `cd apps/hub-dashboard && npx next build` → build succeeds

**Steps:**

- [ ] **Step 1: Pass `capabilities` into `DeviceTabs`**

In `apps/hub-dashboard/src/app/(dashboard)/devices/[id]/page.tsx`, change line 73 from:

```tsx
<DeviceTabs deviceId={device.id} jobs={jobs} online={device.online} />
```

to:

```tsx
<DeviceTabs deviceId={device.id} jobs={jobs} online={device.online} capabilities={device.capabilities} />
```

- [ ] **Step 2: Update `DeviceTabs` component**

In `apps/hub-dashboard/src/components/device-tabs.tsx`, add the import:

```tsx
import { DeviceBrowser } from "./device-browser";
```

Update the props and state:

```tsx
export function DeviceTabs({
  deviceId,
  jobs,
  online,
  capabilities,
}: {
  deviceId: string;
  jobs: Job[];
  online: boolean;
  capabilities: string[];
}) {
  const hasBrowser = online && capabilities.includes("browser");
  const [tab, setTab] = useState<"jobs" | "terminal" | "browser">(
    hasBrowser ? "browser" : online ? "terminal" : "jobs",
  );
```

Add the Browser tab button after the Terminal button:

```tsx
{hasBrowser && (
  <button
    className={`device-tab ${tab === "browser" ? "device-tab-active" : ""}`}
    onClick={() => setTab("browser")}
  >
    Browser
  </button>
)}
```

Add the Browser tab content after the Terminal content:

```tsx
{tab === "browser" && hasBrowser && (
  <DeviceBrowser deviceId={deviceId} />
)}
```

- [ ] **Step 3: Add browser panel CSS**

Append to `apps/hub-dashboard/src/app/globals.css`:

```css
/* ── Browser Panel ─────────────────────────────────────────── */

.browser-panel {
  padding: 16px 24px 24px;
}

.browser-section {
  margin-bottom: 16px;
}

.browser-section-title {
  font-size: 0.85rem;
  font-weight: 600;
  color: var(--muted);
  text-transform: uppercase;
  letter-spacing: 0.05em;
  margin-bottom: 8px;
}

.browser-section-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 8px;
}

.browser-form-row {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 6px;
}

.browser-label {
  font-size: 0.85rem;
  color: var(--muted);
  min-width: 72px;
  white-space: nowrap;
}

.browser-input {
  flex: 1;
  padding: 6px 10px;
  background: rgba(15, 23, 42, 0.6);
  border: 1px solid var(--border);
  border-radius: 6px;
  color: var(--text);
  font-family: "SFMono-Regular", "Menlo", "Monaco", "Consolas", monospace;
  font-size: 0.85rem;
}

.browser-input:focus {
  outline: none;
  border-color: var(--accent);
}

.browser-actions-row {
  display: flex;
  gap: 6px;
  margin-top: 8px;
  flex-wrap: wrap;
}

.browser-btn {
  padding: 5px 12px;
  background: rgba(148, 163, 184, 0.12);
  border: 1px solid var(--border);
  border-radius: 6px;
  color: var(--text);
  font-size: 0.8rem;
  font-weight: 500;
  cursor: pointer;
  transition: background 0.15s;
}

.browser-btn:hover:not(:disabled) {
  background: rgba(148, 163, 184, 0.22);
}

.browser-btn:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}

.browser-btn-primary {
  background: rgba(94, 234, 212, 0.15);
  border-color: rgba(94, 234, 212, 0.3);
  color: var(--accent);
}

.browser-btn-primary:hover:not(:disabled) {
  background: rgba(94, 234, 212, 0.25);
}

.browser-btn-danger {
  background: rgba(248, 113, 113, 0.12);
  border-color: rgba(248, 113, 113, 0.25);
  color: #f87171;
}

.browser-btn-danger:hover:not(:disabled) {
  background: rgba(248, 113, 113, 0.22);
}

.browser-btn-sm {
  padding: 3px 8px;
  font-size: 0.75rem;
}

.browser-log {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.browser-log-entry {
  background: rgba(15, 23, 42, 0.5);
  border: 1px solid var(--border);
  border-radius: 8px;
  overflow: hidden;
}

.browser-log-header {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  background: rgba(15, 23, 42, 0.4);
  font-size: 0.8rem;
}

.browser-log-action {
  font-family: "SFMono-Regular", "Menlo", monospace;
  font-weight: 600;
  color: var(--accent);
  font-size: 0.8rem;
}

.browser-log-params {
  color: var(--muted);
  font-family: "SFMono-Regular", "Menlo", monospace;
  font-size: 0.75rem;
  max-width: 400px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.browser-log-time {
  color: var(--muted);
  font-size: 0.75rem;
  margin-left: auto;
}

.browser-log-body {
  padding: 8px 12px;
  font-size: 0.8rem;
}

.browser-log-loading {
  color: var(--muted);
  font-style: italic;
}

.browser-log-success {
  color: var(--accent);
  font-weight: 600;
}

.browser-log-fail {
  color: #f87171;
}

.browser-log-binary {
  margin-top: 8px;
}

.browser-preview-img {
  max-width: 100%;
  max-height: 400px;
  border-radius: 4px;
  border: 1px solid var(--border);
}

.browser-download-link {
  color: var(--accent);
  text-decoration: underline;
  font-size: 0.8rem;
}

.browser-log-data {
  background: #020617;
  border: 1px solid var(--border);
  border-radius: 4px;
  padding: 8px;
  margin-top: 6px;
  font-family: "SFMono-Regular", "Menlo", monospace;
  font-size: 0.78rem;
  color: #dbeafe;
  overflow-x: auto;
  white-space: pre-wrap;
  word-break: break-all;
  max-height: 300px;
  overflow-y: auto;
}
```

- [ ] **Step 4: Verify build**

Run: `cd apps/hub-dashboard && npx next build`
Expected: Build succeeds

- [ ] **Step 5: Commit**

```bash
git add apps/hub-dashboard/src/app/\(dashboard\)/devices/\[id\]/page.tsx apps/hub-dashboard/src/components/device-tabs.tsx apps/hub-dashboard/src/app/globals.css
git commit -m "feat(dashboard): integrate Browser tab with capability gate and styles"
```
