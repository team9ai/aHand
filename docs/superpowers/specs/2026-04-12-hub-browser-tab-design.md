# Hub Browser Tab Design Spec

**Date:** 2026-04-12
**Status:** Draft

## Overview

Add browser automation support to the hub, enabling the hub-dashboard to send browser commands (open, click, snapshot, screenshot, etc.) to devices via a new `/api/browser` endpoint. This includes a "Browser" tab in the device detail page that mirrors the functionality of the old SolidJS dashboard's BrowserPanel.

## Motivation

The old dashboard (`apps/dashboard/`) has a BrowserPanel that works through `dev-cloud` (Hono + SDK). The new hub-dashboard currently only supports terminal (job execution). Browser automation needs to go through the hub (Rust/Axum) to maintain architectural consistency.

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| API location | Hub Rust backend (`/api/browser`) | Architectural consistency — all device commands go through hub |
| Binary data transport | Base64 inline in JSON | Simple, matches old dashboard behavior |
| Tab visibility | Online + `"browser"` capability | Only show when device actually supports browser |
| UI scope | Full port of old BrowserPanel | All actions: open, click, fill, snapshot, screenshot, pdf, download, close, custom |

## Part 1: Hub Backend (Rust)

### New Endpoint

**Route:** `POST /api/browser`

**Request:**

```json
{
  "device_id": "uuid",
  "session_id": "string",
  "action": "open | click | fill | snapshot | screenshot | pdf | download | close | ...",
  "params": { "url": "...", "selector": "..." },  // optional, default {}
  "timeout_ms": 30000                              // optional, default 30000
}
```

**Response:**

```json
{
  "success": true,
  "data": {},
  "error": null,
  "binary_data": "<base64 string or null>",
  "binary_mime": "image/png"
}
```

### Implementation

1. Validate auth (existing middleware)
2. Look up device connection by `device_id` in `AppState`
3. Build `BrowserRequest` protobuf, wrap in `Envelope`
4. Send via device's WebSocket connection
5. Await `BrowserResponse` via oneshot channel with timeout
6. Base64-encode `binary_data`, return JSON

### Files to modify

- `crates/ahand-hub/src/http/mod.rs` — add route
- `crates/ahand-hub/src/http/browser.rs` — new handler module
- `crates/ahand-hub/src/state.rs` — add `BrowserPendingMap`
- `crates/ahand-hub/src/ws/device_gateway.rs` — handle `BrowserResponse` payloads

## Part 2: Frontend (hub-dashboard)

### DeviceTabs Changes

- Extend tab type: `"jobs" | "terminal" | "browser"`
- Browser tab visible when: `online && capabilities.includes("browser")`
- Pass `capabilities: string[]` from device detail page into `DeviceTabs`

### DeviceBrowser Component

Port from old `apps/dashboard/src/panels/BrowserPanel.tsx` (SolidJS → React).

**Sections:**

1. **Session config** — Session ID input (default `"test-session"`)
2. **Actions** — URL + Open, Selector, Value + Fill, button row (Snapshot, Click, Fill, Screenshot, Download, PDF, Close)
3. **Custom Command** (collapsible) — Action name, Params JSON, Send button
4. **Response Log** — Reverse-chronological entries with:
   - Action name + params + timestamp
   - Success/failure status
   - Inline image preview for screenshots
   - Download link for PDFs/non-image binaries
   - Formatted JSON for data responses
   - Clear button

**API call:** `POST` to `buildProxyUrl("/api/browser")` — synchronous request-response through existing Next.js proxy.

### Files to create/modify

- `apps/hub-dashboard/src/components/device-browser.tsx` — new component
- `apps/hub-dashboard/src/components/device-tabs.tsx` — add browser tab
- `apps/hub-dashboard/src/app/(dashboard)/devices/[id]/page.tsx` — pass capabilities
- `apps/hub-dashboard/src/app/globals.css` — browser panel styles

## Part 3: Hub Internal Message Routing

### Device Gateway Extension

**New state in `AppState`:**

```rust
type BrowserPendingMap = Arc<DashMap<String, oneshot::Sender<BrowserResponse>>>;
```

- Key: `request_id`
- Value: oneshot sender waiting for response

**Send flow (HTTP handler → device):**

1. HTTP handler generates `request_id`, creates `oneshot::channel()`
2. Insert sender into `BrowserPendingMap`
3. Build `Envelope { payload: BrowserRequest }`, send via device WebSocket tx
4. `await receiver` with timeout

**Receive flow (device → HTTP handler):**

1. Device gateway receives `Envelope` with `BrowserResponse`
2. Look up `request_id` in `BrowserPendingMap`, remove entry
3. `sender.send(response)` to wake up HTTP handler

**Timeout:** Default 30s, configurable via `timeout_ms`. On timeout, remove from pending map, return HTTP 504.

## Part 4: Data Flow & Error Handling

### Complete Data Flow

```
hub-dashboard Browser Tab
  → POST /api/proxy/api/browser (Next.js proxy)
  → Hub POST /api/browser (new)
  → Validate auth → find device connection → create oneshot channel
  → Build Envelope(BrowserRequest) → send via WS to device
  → Daemon: session check → domain check → BrowserManager::execute()
  → Daemon: spawn playwright-cli → collect result
  → BrowserResponse via WS → gateway matches request_id → oneshot send
  → Hub HTTP handler: base64 encode binary_data → return JSON
  → Dashboard renders result
```

### Error Handling

| Scenario | HTTP Status | Response |
|----------|-------------|----------|
| Device not online | 404 | `"device not connected"` |
| Device lacks browser capability | 400 | `"device does not support browser"` |
| Request timeout | 504 | `"browser command timed out"` |
| Auth failure | 401 | (standard) |
| Device-side business error | 200 | `{success: false, error: "..."}` |

Device-side errors (domain policy denied, session mode inactive, playwright-cli missing, etc.) return HTTP 200 with `{success: false, error: "..."}` — consistent with old dashboard behavior.

## Why Browser Is Not a Job

Browser commands use separate `BrowserRequest`/`BrowserResponse` protobuf messages rather than the Job system because:

1. **Response pattern:** Jobs stream multiple events (stdout/stderr chunks → finished); browser returns a single atomic response
2. **Binary data:** Jobs only carry text; browser has dedicated `binary_data` + `binary_mime` fields
3. **Stateful sessions:** Browser has persistent `session_id` (page state, cookies, navigation); jobs are stateless
4. **Idempotency:** Jobs cache results by `job_id`; browser always executes fresh
5. **Cancellation:** Jobs can be cancelled mid-execution; browser commands are atomic
