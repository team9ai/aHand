# Remote Desktop Control — Design & Phase 1 Plan

**Date:** 2026-04-12
**Status:** Design locked in, implementation pending.
**Scope:** Remote desktop view/control capability for aHand, debugged first in hub-dashboard. Phase 1 target is a macOS view-only prototype.

## Goal

Add remote desktop viewing and (eventually) control to aHand so an operator can see a device's screen in real time from hub-dashboard and send input back. Phase 1 is a **macOS view-only** prototype validating the end-to-end plumbing (capture → envelope → hub → canvas). Later phases graduate to input forwarding, multi-OS, production encoding, and window-scoped capture.

aHand's hub already tunnels all device traffic through a persistent WebSocket, so NAT traversal / P2P / relay — the 50% of RustDesk's complexity — is solved for us. We inherit the good parts of RustDesk's design (per-platform capture, codec negotiation, input injection via `enigo`) without its networking layer.

## Quick Decisions Reference

| # | Decision | Value |
|---|---|---|
| Q1 | Phase 1 scope | **view-only** (no input injection) |
| Q2 | Phase 1 OS | **macOS only** (Linux X11 / Windows → Phase 2, Wayland → Phase 2.5) |
| Q3 | Protocol / data model | **hybrid** — control plane reuses `JobRequest`/`JobFinished`, data plane is dedicated `DesktopFrame` envelope payload |
| Q4.1 | Capture crate | `xcap` 0.7 |
| Q4.2 | Frame format | JPEG (quality 70, default 10 fps) |
| Q4.3 | JPEG encoder | `image` crate built-in |
| Q4.4 | Browser renderer | `<canvas>` + `createImageBitmap` + `drawImage` |
| Q4.5 | Hub frame routing | **new `DesktopFrameBus`**, bypasses `OutputStream`, live-only broadcast channel, drops frames on slow subscriber |
| Q4.6 | Session concurrency | one active desktop session per device; second attempt → 409 Conflict |
| Q4.7 | macOS Screen Recording permission | no UX in Phase 1; fail with clear error message, document the one-time system-settings step |
| Q4.8 | Policy integration | new dedicated `allow_desktop_capture` flag, default `false`, must be explicitly opted in |
| Q5.1 | UI location | new "Desktop" tab in `device-tabs.tsx` (sibling of Terminal) |
| Q5.2 | Multi-monitor | `display_index` field exists in Phase 1 but is hardcoded to 0 (primary); picker UI is Phase 1.9 |
| Q5.3 | Debug panel | **permanent requirement, not Phase 1-only** — hub-dashboard is an operator tool, debug surfaces always ship |
| Q6 | Browser vs generic window capture | CDP-based browser live view (roadmap Section 2) handles browser; generic OS window capture (Phase 2W) handles non-browser windows |

## Existing aHand Infrastructure Reused

| Layer | What we reuse |
|---|---|
| Protocol (protobuf envelope) | seq/ack reliable transport, binary frames, payload oneof |
| Hub device gateway `/ws` | Bidirectional device ↔ hub transport |
| Hub dashboard gateway pattern (`/ws/terminal` + one-time token) | New `/ws/desktop` mirrors this exactly |
| Daemon job/executor framework | Start/stop/timeout lifecycle, policy enforcement, audit logging |
| `JobRegistry` cancel/permit mechanism | Reused verbatim |
| Hub-dashboard WebSocket + auth | JWT session cookie, token endpoint |

A desktop session is modeled as "a new kind of job" — same way interactive terminal was added on top of pipe mode via a `JobRequest.interactive` flag. We add `JobRequest.desktop_capture` similarly.

## Approach Comparison (for context)

Three approaches were evaluated before settling on Phase 1's JPEG path.

### Option A — Screenshot polling + JPEG (chosen for Phase 1)

Daemon polls the screen at ~10 fps, encodes each frame as JPEG, sends it as a binary envelope. Dashboard renders to `<canvas>`.

- **Pros:** Minimal dependencies (`xcap` + `image`), simplest possible end-to-end path, easy to reason about.
- **Cons:** 200ms+ latency, high bandwidth, no incremental encoding.
- **Role:** Bring-up prototype to validate transport, rendering, permission handling.

### Option B — noVNC + VNC Server on device (rejected)

Run a VNC Server on the device, tunnel the VNC TCP stream through the aHand WebSocket, embed noVNC in dashboard.

- **Pros:** Incremental encoding, input forwarding, clipboard sync all "free" from VNC.
- **Cons:** External dependency (VNC Server must be installed); headless servers need Xvfb; macOS requires Screen Recording permission AND an installed VNC Server; less flexibility for future codec choices.
- Kept as a fallback option if Option C (Phase 2) hits blockers.

### Option C — RustDesk-style: libvpx encoding + WebCodecs decoding (chosen for Phase 2)

Capture via per-platform APIs, encode with libvpx (VP9) or libaom (AV1) in realtime/CBR mode, transport raw frames through envelopes, decode in the browser with WebCodecs `VideoDecoder`.

- **Pros:** 30-80ms latency, low bandwidth, browser-native decode.
- **Cons:** Larger implementation surface — Rust libvpx bindings, per-platform capture polish, codec negotiation.
- **Role:** Phase 2 production encoding target. Inherits RustDesk's codec design (VP9 default, CBR realtime mode) without its rendezvous/relay/NAT complexity (aHand doesn't need any of that).

## RustDesk Reference Points

Technical reference pulled from the `rustdesk/rustdesk` source tree. We **borrow ideas**, not architecture:

- **Capture backends:** the `rustdesk/libs/scrap` crate has the cleanest open-source per-platform capture code (DXGI for Windows, CGDisplayStream for macOS, XCB SHM for X11, PipeWire/portal for Wayland). Phase 1 uses `xcap` (simpler, actively maintained); Phase 2+ may vendor parts of `scrap` if performance demands it.
- **Codec path:** VP9 via libvpx, realtime/CBR mode, `rc_dropframe_thresh=25`, cpu-speed 5-8. These tuning knobs are our Phase 2 starting point.
- **Input injection:** RustDesk uses its own `enigo` fork for Windows/macOS/Linux X11, `uinput` helper process for Wayland headless, and xdg-portal RemoteDesktop for Wayland with display. We'll start with the `enigo` crate proper when we get to Phase 1.5.
- **Adaptive QoS:** `src/server/video_qos.rs` dynamically tunes fps/bitrate/keyframe interval based on client feedback. Phase 3 target.
- **What we explicitly reject:** rendezvous server (we have hub), relay server (ditto), KCP reliable-UDP (we have WebSocket), NAT type detection (no P2P), sodium handshake (we have JWT + TLS), Flutter UI (we have Next.js), web client via JS shim (we have native React).

## Section 1 — Protocol Layer

Control plane reuses `JobRequest` / `JobFinished` / `CancelJob` (free lifecycle, audit, policy). Data plane is a dedicated `DesktopFrame` envelope payload.

### 1.1 Extend `JobRequest` with an optional `desktop_capture` config

In [proto/ahand/v1/envelope.proto](../../../proto/ahand/v1/envelope.proto):

```protobuf
message JobRequest {
  string job_id = 1;
  string tool   = 2;
  repeated string args = 3;
  string cwd    = 4;
  map<string, string> env = 5;
  uint64 timeout_ms = 6;
  bool   interactive = 7;

  // If present, this is a desktop capture session instead of an exec job.
  // When set, tool/args/cwd/env/interactive are ignored by the daemon.
  DesktopCaptureConfig desktop_capture = 8;
}

message DesktopCaptureConfig {
  uint32 fps           = 1;  // target frame rate; 0 = default (10)
  uint32 jpeg_quality  = 2;  // 1-100; 0 = default (70)
  uint32 display_index = 3;  // 0 = primary; reserved for Phase 1.9 multi-monitor UI
}
```

**Why an optional sub-message rather than a new `JobKind` enum:** adding `JobKind` would make every existing job become "implicit EXEC kind" — a breaking change to a tag-0 field is incompatible with older daemons. An optional sub-message extends cleanly: old daemons ignore the unknown field, new daemons check `desktop_capture.is_some()`.

**Why not abuse `tool = "__ahand_desktop__"`:** putting fps / quality in args as string flags requires parsing and pollutes args semantics. A structured sub-message is cleaner.

### 1.2 New data-plane message `DesktopFrame`

```protobuf
// DesktopFrame — streamed daemon → hub during an active capture session.
// Carries a single encoded frame. Tied to an existing desktop-capture job via job_id.
message DesktopFrame {
  string job_id         = 1;
  uint64 frame_id       = 2;  // monotonic, 0-based
  uint32 width          = 3;
  uint32 height         = 4;
  string mime           = 5;  // "image/jpeg" in Phase 1; kept as field for future WebP/VP9
  bytes  data           = 6;  // encoded frame payload
  uint64 captured_at_ms = 7;  // daemon wall-clock, for latency display in debug panel
}
```

### 1.3 Envelope oneof extension

```protobuf
message Envelope {
  // ...existing fields...
  oneof payload {
    // ...existing 17 variants...
    StdinChunk     stdin_chunk     = 29;
    TerminalResize terminal_resize = 30;
    DesktopFrame   desktop_frame   = 31;  // NEW
  }
}
```

### 1.4 Policy protocol additions

```protobuf
message PolicyState {
  // ...existing fields...
  uint64 approval_timeout_secs = 5;
  bool   allow_desktop_capture = 6;  // NEW, default false
}

// PolicyUpdate uses sentinel values for "don't change" rather than proto3
// `optional`, matching the existing convention (e.g., approval_timeout_secs = 0).
// For tri-state booleans we use a dedicated enum with UNCHANGED = 0.
enum DesktopCaptureAllowUpdate {
  DESKTOP_CAPTURE_ALLOW_UNCHANGED = 0;  // default; no change
  DESKTOP_CAPTURE_ALLOW_DENY      = 1;
  DESKTOP_CAPTURE_ALLOW_GRANT     = 2;
}

message PolicyUpdate {
  // ...existing fields...
  uint64 approval_timeout_secs = 9;
  DesktopCaptureAllowUpdate set_allow_desktop_capture = 10;  // NEW
}
```

### 1.5 Phase 1 message sequence

```
Hub                                           Daemon
 │                                               │
 │ ── JobRequest {                               │
 │      job_id: "abc",                           │
 │      desktop_capture: {                       │
 │        fps: 10, jpeg_quality: 70,             │
 │        display_index: 0                       │
 │      }                                        │
 │    } ────────────────────────────────────────▶│
 │                                               │ (policy check → pass)
 │                                               │ (xcap init → spawn capture loop)
 │◀──────────────────────────── DesktopFrame 0 ──│
 │◀──────────────────────────── DesktopFrame 1 ──│
 │◀──────────────────────────── DesktopFrame 2 ──│
 │                 ...                           │
 │ ── CancelJob { job_id: "abc" } ──────────────▶│
 │                                               │ (stop capture loop)
 │◀─────────────── JobFinished { exit_code: 0 } ─│
```

Rejection paths reuse `JobRejected`; policy failures use the existing policy flow. No new control messages required.

### 1.6 Messages deferred to later phases (explicit)

| Message | Phase | Purpose |
|---|---|---|
| `DesktopInputEvent` (oneof: MouseMove / MouseButton / MouseScroll / KeyDown / KeyUp) | 1.5 | Mouse/keyboard input from dashboard to device |
| `DesktopDisplayInfo` query + response | 1.9 | Enumerate displays on the device for picker UI |
| `DesktopCaptureConfig.target` oneof (display vs window) + `WindowRef`; retire flat `display_index` via `reserved 3` and move it into the oneof at new tag | 2W | Window-scoped capture — generic path (browser uses CDP in Section 2 of roadmap) |
| `DesktopKeyframeRequest` | 2 | Ask daemon for a keyframe after subscriber (re)connect; meaningless for JPEG, needed for VP9 |
| `DesktopCodecNegotiate` + codec field in `DesktopCaptureConfig` | 2 | JPEG → VP9 switch |
| `DesktopSessionStats` | 3 | Adaptive QoS feedback channel |

## Section 2 — Daemon Layer

New module for desktop capture, routed from the existing `spawn_job` dispatcher.

### 2.1 New module [crates/ahandd/src/desktop.rs](../../../crates/ahandd/src/desktop.rs)

Independent file — do not merge into the already-504-line `executor.rs`. Single responsibility: capture loop.

```rust
use crate::executor::EnvelopeSink;
use ahand_protocol::{DesktopCaptureConfig, DesktopFrame, Envelope, envelope};
use tokio::sync::mpsc;

/// Run a desktop capture session. Captures the primary display at `config.fps`,
/// encodes each frame as JPEG at `config.jpeg_quality`, ships each via `tx`.
/// Terminates when `cancel_rx` fires or the capture backend errors repeatedly.
///
/// Returns `(exit_code, error)` so the caller can reuse the same JobRegistry
/// cleanup plumbing as `run_job` / `run_job_pty`.
pub async fn run_desktop_capture<T>(
    device_id: String,
    job_id: String,
    config: DesktopCaptureConfig,
    tx: T,
    mut cancel_rx: mpsc::Receiver<()>,
) -> (i32, String)
where
    T: EnvelopeSink,
{
    // 1. Resolve effective config (defaults: fps=10, jpeg_quality=70, display_index=0)
    // 2. Init xcap capturer for the selected display; on failure → early return with
    //    platform-specific error message
    // 3. Capture loop:
    //    - tokio::select! between cancel_rx.recv() and interval tick (1000/fps ms)
    //    - On tick: xcap capture → spawn_blocking JPEG encode → build DesktopFrame
    //      → wrap in envelope → tx.send
    //    - On cancel: break loop, return (0, "")
    //    - On single-frame error: log warn, skip, continue
    //    - On 3 consecutive errors: return (1, "capture loop failed: ...")
    // 4. Final JobFinished is emitted by the caller (ahand_client.rs), not here
}
```

**Key design decisions:**

- **tokio interval, not std::thread::sleep** — stay async-friendly.
- **JPEG encoding inside `spawn_blocking`** — the `image` crate is synchronous; running it on the async runtime would block other tasks.
- **Three-tier failure policy:**
  - Single frame encode/capture error → warn log, drop frame, continue
  - 3 consecutive errors → backend considered broken, session terminates
  - Initialization failure (permission denied, no display) → immediate error return
- **No back-pressure logic in Phase 1** — `tx.send` is unbounded; if the outbox saturates the WS layer handles it. Phase 2 can add daemon-side drop logic.
- **No `RunStore` parameter** — desktop frames are not stdout/stderr trace data; they don't need on-disk persistence.

### 2.2 `ahand_client.rs::spawn_job` — new desktop branch

Currently the dispatcher in [crates/ahandd/src/ahand_client.rs](../../../crates/ahandd/src/ahand_client.rs) branches on `req.interactive` to choose between `run_job_pty` and `run_job`. Add a third branch ahead of those:

```rust
if let Some(dc_config) = req.desktop_capture.clone() {
    // Desktop capture path
    reg.register(job_id.clone(), cancel_tx).await;
    let active = reg.active_count().await;
    info!(job_id = %job_id, active_jobs = active, kind = "desktop", "desktop capture accepted");

    tokio::spawn(async move {
        let _permit = reg.acquire_permit().await;
        let (exit_code, error) = desktop::run_desktop_capture(
            did, job_id.clone(), dc_config, tx_clone, cancel_rx
        ).await;
        reg.remove(&job_id).await;
        reg.mark_completed(job_id, exit_code, error).await;
    });
} else if interactive {
    // ... existing PTY branch ...
} else {
    // ... existing pipe branch ...
}
```

The desktop branch uses plain `register` (not `register_interactive`) because Phase 1 view-only has no stdin channel. Phase 1.5 will add a `register_desktop` variant that carries an input event channel.

### 2.3 Policy integration

Current `policy.rs` has `allowed_tools` / `denied_tools` / `denied_paths` / `allowed_domains`. Desktop capture isn't "executing a tool," so it gets a **dedicated** policy dimension rather than being shoehorned into tool allowlists.

**Policy check point:** inside `spawn_job`, before registering the job:

```rust
if req.desktop_capture.is_some() {
    if !policy.allow_desktop_capture() {
        send_rejected(&tx, job_id, "desktop capture not permitted by policy");
        return;
    }
}
```

**Config change:** `crates/ahandd/src/config.rs` → `PolicyConfig` gets a new field:

```rust
pub struct PolicyConfig {
    // ...existing fields...
    #[serde(default)]
    pub allow_desktop_capture: bool,  // default false
}
```

And `ahandd.toml`:

```toml
[policy]
allow_desktop_capture = false  # must be explicitly opted in per-device
```

### 2.4 macOS Screen Recording permission handling

First call to `xcap::Monitor::all()` on macOS without permission returns an error (or a completely black frame, depending on xcap version). Handling:

- **Init failure path:** `Capturer::new()` error → `run_desktop_capture` returns `(1, "screen recording permission denied: grant access in System Settings → Privacy & Security → Screen Recording, then restart ahandd")`. Hub propagates this error to dashboard, where the operator sees a clear message.
- **Runtime black-frame detection:** Phase 1 does **not** detect this. Trust `xcap`'s return value. If the operator sees a black screen on dashboard, they go check permissions.
- **Documentation:** [crates/ahandd/README.md](../../../crates/ahandd/README.md) gains a "macOS Screen Recording Permission" section explaining the one-time system-settings flow.

### 2.5 New dependencies

`crates/ahandd/Cargo.toml`:

```toml
[dependencies]
# ...existing...
xcap  = "0.7"
image = "0.25"
```

Use `image`'s built-in JPEG encoder first. Upgrade to `turbojpeg` only if Phase 1 load testing shows CPU bottleneck. Implementation note: check the workspace for existing `image` crate presence; if already transitive-depended, reuse that version instead of pinning a new one.

### 2.6 Phase 1 daemon "done" criteria

- [ ] `run_desktop_capture` exits within 100ms of cancel signal
- [ ] 3 consecutive frame errors terminate the session
- [ ] macOS permission denial returns a clear, user-readable error string
- [ ] Policy defaults to deny; opt-in via config flag works
- [ ] Unit tests: cancel behavior, default values, error aggregation
- [ ] Integration test: JobRequest → N frames → CancelJob → JobFinished loop (with fake `EnvelopeSink`)

## Section 3 — Hub Layer

Frame data flows `device_gateway` → `handle_device_frame` → new **live-only broadcast bus** → new `/ws/desktop` gateway → dashboard. Control plane fully reuses `/api/jobs`.

### 3.1 New module [crates/ahand-hub/src/desktop_bus.rs](../../../crates/ahand-hub/src/desktop_bus.rs)

**Explicitly does NOT go through `OutputStream`.** Reasons:
- `OutputStream` is designed for "append-only + history replay + Redis persistence"; desktop frames need none of that.
- A frame is ~30-300 KB; at 10 fps persisting to Redis would write ~60-600 MB/minute — catastrophic.
- Old frames are useless; slow subscribers should drop frames, not replay history.

```rust
use ahand_protocol::DesktopFrame;
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

const CHANNEL_CAPACITY: usize = 4;  // small: if subscriber lags > 4 frames, drop

pub struct DesktopFrameBus {
    sessions: DashMap<String, broadcast::Sender<Arc<DesktopFrame>>>,
}

impl DesktopFrameBus {
    pub fn new() -> Self { /* ... */ }

    /// Called by the device gateway when a DesktopFrame arrives.
    /// Returns the number of live subscribers.
    pub fn publish(&self, job_id: &str, frame: DesktopFrame) -> usize { /* ... */ }

    /// Called by the /ws/desktop gateway when a client subscribes.
    /// Creates a new session channel on first subscribe.
    pub fn subscribe(&self, job_id: &str) -> broadcast::Receiver<Arc<DesktopFrame>> { /* ... */ }

    /// Remove the session when the desktop job terminates.
    pub fn close(&self, job_id: &str) { /* ... */ }
}
```

**Key semantics:**
- `broadcast::channel(4)` — capacity of 4 frames. A slow subscriber that lags > 4 receives `RecvError::Lagged(n)` — exactly what we want (drop, don't block).
- `Arc<DesktopFrame>` — shared pointer so multiple subscribers don't copy payload bytes.
- **No persistence.** Channels live in-memory in the DashMap.
- `subscribe` can happen before the first frame (creates an empty channel).

### 3.2 `handle_device_frame` extension

In [crates/ahand-hub/src/http/jobs.rs:191](../../../crates/ahand-hub/src/http/jobs.rs#L191), add a match arm:

```rust
Some(ahand_protocol::envelope::Payload::DesktopFrame(frame)) => {
    let Some(job) = self.job_for_device(device_id, &frame.job_id).await? else {
        anyhow::bail!("desktop frame for unknown job {}", frame.job_id);
    };
    self.clear_disconnect_task(&frame.job_id);
    if is_terminal_status(job.status) {
        self.connections.observe_inbound(device_id, seq, ack)?;
        return Ok(());
    }
    // First frame transitions Pending → Running (mirror the stdout_chunk path)
    if let Err(err) = self.transition_job(
        &frame.job_id, JobStatus::Running, &format!("device:{device_id}")
    ).await {
        return self.handle_stale_device_frame_error(device_id, seq, ack, err);
    }
    // Route to the live-only bus, NOT the output_stream
    let subscribers = self.desktop_bus.publish(&frame.job_id, frame);
    tracing::debug!(job_id = %frame.job_id, subscribers, "desktop frame routed");
}
```

The `JobFinished` arm gains a single line: if the finishing job is a desktop-capture job, call `self.desktop_bus.close(&finished.job_id)`.

### 3.3 New HTTP endpoints `/api/desktop/token` and `/ws/desktop`

**New file [crates/ahand-hub/src/http/desktop.rs](../../../crates/ahand-hub/src/http/desktop.rs)** — mirrors [terminal.rs](../../../crates/ahand-hub/src/http/terminal.rs) structurally. Phase 1 view-only removes the dashboard → device forwarding path entirely.

```rust
pub struct DesktopToken { /* same shape as TerminalToken */ }

#[derive(Deserialize)]
pub struct CreateTokenRequest { pub job_id: String }

#[derive(Serialize)]
pub struct CreateTokenResponse { pub token: String, pub expires_in: u64 }

// POST /api/desktop/token
// Auth: JWT session cookie (same as /api/terminal/token).
// Verifies the job exists, is a desktop_capture job, and belongs to the caller.
// Returns a one-time token, 60s TTL.
pub async fn create_token(...) -> Result<Json<CreateTokenResponse>, ApiError>;

// GET /ws/desktop?token=...&job_id=...
// Auth: the one-time token.
// Wire format: text metadata frame + binary JPEG frame pairs.
pub async fn handle_desktop_ws(...) -> Response;
```

### 3.4 Wire format on `/ws/desktop` (Phase 1)

JPEG binary data + JSON metadata, sent as paired WebSocket frames:

```
→ (TEXT)   {"type":"frame","frame_id":42,"width":2560,"height":1600,"captured_at_ms":1712345678901}
→ (BINARY) <raw JPEG bytes>
→ (TEXT)   {"type":"frame","frame_id":43,...}
→ (BINARY) <raw JPEG bytes>
...
→ (TEXT)   {"type":"ended","exit_code":0}
```

**Rejected alternatives:**
- **JPEG base64 inside JSON** — 33% bandwidth overhead + CPU cost for encode/decode. Rejected.
- **Raw protobuf forwarding** — frontend would need a protobuf dependency for one message type. Rejected.
- **Single binary frame with a length-prefixed header** — forces frontend to parse its own mini format. The JSON-text + binary pair is idiomatic WebSocket.

Dashboard side: receive text → parse metadata → hold pending; receive binary → use pending metadata to render, clear pending.

### 3.5 Concurrency control — one session per device

In the `POST /api/jobs` handler, before accepting a desktop-capture request:

```rust
if new_req.desktop_capture.is_some() {
    let active = self.jobs_by_device(&new_req.device_id).await?
        .into_iter()
        .filter(|j| j.is_desktop_capture() && !is_terminal_status(j.status))
        .count();
    if active >= 1 {
        return Err(ApiError::conflict("device already has an active desktop session"));
    }
}
```

`is_desktop_capture()` is a helper on the job record — see 3.6.

### 3.6 Jobs table — new `desktop_capture_config` column

JSONB (nullable). Stores `{fps, jpeg_quality, display_index}` for:
- Filtering desktop jobs from regular jobs in list/query
- Audit log completeness (operator can see what config was used)
- Phase 2 codec negotiation results will land here too

Migration: `add_desktop_capture_config_to_jobs.sql` — additive, backwards-compatible (existing jobs have NULL).

### 3.7 Route registration

[crates/ahand-hub/src/http/mod.rs](../../../crates/ahand-hub/src/http/mod.rs):

```rust
.route("/api/desktop/token", post(desktop::create_token))
.route("/ws/desktop", get(desktop::handle_desktop_ws))
```

### 3.8 Phase 1 hub "done" criteria

- [ ] `/api/desktop/token` issues one-time tokens (60s TTL), single-use enforced
- [ ] `/ws/desktop` subscribers receive text metadata + binary JPEG frame pairs
- [ ] Same-device concurrent desktop session creation returns 409 Conflict
- [ ] Session end (`JobFinished`) causes WS to emit `{"type":"ended"}` and close cleanly
- [ ] Unit tests: `DesktopFrameBus` publish/subscribe/close/lag behavior
- [ ] Integration test: fake device publishes → subscriber receives; slow subscriber drops without blocking publisher
- [ ] Migration applies cleanly against existing dev DB

## Section 4 — Dashboard Layer

Desktop tab in hub-dashboard: creates a desktop job, subscribes to the frame stream, renders to canvas, shows a permanent debug panel.

### 4.1 Component split

| File | Responsibility | Est. LOC |
|---|---|---|
| [components/device-desktop.tsx](../../../apps/hub-dashboard/src/components/device-desktop.tsx) | Top-level component: session lifecycle, wires hook + children | ~200 |
| [components/device-desktop-canvas.tsx](../../../apps/hub-dashboard/src/components/device-desktop-canvas.tsx) | Pure renderer: receives frames via ref callback, manages canvas | ~80 |
| [components/device-desktop-debug.tsx](../../../apps/hub-dashboard/src/components/device-desktop-debug.tsx) | Debug panel: fps / bytes / latency / resolution / status badge | ~80 |
| [hooks/use-desktop-stream.ts](../../../apps/hub-dashboard/src/hooks/use-desktop-stream.ts) | WS hook + state machine + stats aggregation | ~180 |

**Why split this aggressively:** [device-terminal.tsx](../../../apps/hub-dashboard/src/components/device-terminal.tsx) is already 857 lines. Phase 1.5 will add input handling, Phase 2 will swap the codec — each of those edits wants a focused file, not a mega-component.

### 4.2 `use-desktop-stream.ts` state machine

```typescript
export type DesktopFrameMeta = {
  frameId: number;
  width: number;
  height: number;
  capturedAtMs: number;
  receivedAtMs: number;
  byteLength: number;
};

export type DesktopStreamState =
  | { kind: "idle" }
  | { kind: "requesting-token" }
  | { kind: "connecting" }
  | { kind: "streaming"; lastFrame: DesktopFrameMeta | null }
  | { kind: "ended"; exitCode: number; reason?: string }
  | { kind: "error"; message: string };

export type DesktopStats = {
  fpsEma: number;          // EMA of frame arrival rate
  lastFrameBytes: number;
  totalBytes: number;
  frameCount: number;
  droppedCount: number;    // hub-level lag events (future)
  latencyMsEma: number;    // EMA of receivedAtMs - capturedAtMs
};

export type UseDesktopStreamReturn = {
  state: DesktopStreamState;
  stats: DesktopStats;
  start: (opts?: { fps?: number; jpegQuality?: number }) => void;
  stop: () => void;
  /** Register a frame-paint callback. Returns an unsubscribe fn. */
  onFrame: (cb: (bitmap: ImageBitmap, meta: DesktopFrameMeta) => void) => () => void;
};
```

**State transitions:**
1. `idle` → user clicks Start → POST `/api/proxy/api/jobs` with `desktop_capture` → `requesting-token`
2. `requesting-token` → POST `/api/proxy/api/desktop/token` → receive token → `connecting`
3. `connecting` → open `WebSocket(/ws/desktop?token=...&job_id=...)` → first frame → `streaming`
4. `streaming` → (text meta + binary JPEG pairs) → `createImageBitmap(blob)` → fire `onFrame` callbacks → update stats → stay `streaming`
5. Receive `{"type":"ended"}` → `ended`
6. WS closes unexpectedly → `error` (no auto-reconnect in Phase 1)
7. `stop()` → POST `/api/proxy/api/jobs/{id}/cancel` → close WS → `idle`

**Frame parsing:** metadata and binary arrive as paired WS frames. Hook holds a `pendingMeta: DesktopFrameMeta | null`:
- Text frame → parse JSON → if `type === "frame"` set `pendingMeta`; if `type === "ended"` transition to ended state
- Binary frame → if no `pendingMeta` log a warn (protocol error, non-fatal) → otherwise `createImageBitmap` + fire callbacks + clear `pendingMeta`

**EMA stats:** $\alpha = 0.2$, reset on session change.

### 4.3 `device-desktop.tsx` top-level

```tsx
export function DeviceDesktop({ deviceId }: { deviceId: string }) {
  const stream = useDesktopStream(deviceId);
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    return stream.onFrame((bitmap, meta) => {
      const canvas = canvasRef.current;
      const ctx = canvas?.getContext("2d");
      if (!canvas || !ctx) return;
      if (canvas.width !== meta.width || canvas.height !== meta.height) {
        canvas.width = meta.width;
        canvas.height = meta.height;
      }
      ctx.drawImage(bitmap, 0, 0);
      bitmap.close();  // critical — release GPU memory
    });
  }, [stream]);

  // Cleanup on unmount (page change, tab change): stop session
  useEffect(() => () => stream.stop(), [stream]);

  return (
    <div className="device-desktop-panel">
      <DeviceDesktopControls state={stream.state} onStart={stream.start} onStop={stream.stop} />
      <DeviceDesktopCanvas ref={canvasRef} state={stream.state} />
      <DeviceDesktopDebug stats={stream.stats} state={stream.state} />
    </div>
  );
}
```

**`bitmap.close()` is mission-critical.** Forgetting this leaks GPU textures; at 10 fps a 10-minute session leaks 6000 bitmaps and Chrome will crash. This is a well-known WebCodecs/ImageBitmap pitfall.

**Canvas sizing:** internal pixel dimensions match the capture resolution (e.g., 2560×1600); CSS width is `100%` with `object-fit: contain` to scale into the tab area.

### 4.4 Debug panel layout

Compact single-row format per the agreed convention:

```
[🟢 streaming]  frame #1234  ·  fps: 9.8  ·  last: 87 KB  ·  total: 42.1 MB  ·  latency: ~18 ms  ·  res: 2560×1600
```

Status badge colors:
- 🔘 idle (gray)
- 🟡 requesting-token / connecting (yellow)
- 🟢 streaming (green)
- ⚫ ended (dark)
- 🔴 error (red)

Clicking the badge expands a small panel showing: recent 5 errors, WS URL, job ID, session config. **This expansion panel ships in Phase 1** — debugging a stalled desktop session without those values is painful.

### 4.5 `device-tabs.tsx` minimal change

```tsx
const [tab, setTab] = useState<"jobs" | "terminal" | "desktop">(online ? "terminal" : "jobs");
// ... existing ...
{online && (
  <button
    className={`device-tab ${tab === "desktop" ? "device-tab-active" : ""}`}
    onClick={() => setTab("desktop")}
  >
    Desktop
  </button>
)}
{tab === "desktop" && online && <DeviceDesktop deviceId={deviceId} />}
```

### 4.6 API proxy & WebSocket connectivity

The new `POST /api/desktop/token` goes through the existing Next.js proxy at [apps/hub-dashboard/src/app/api/proxy/[...path]/route.ts](../../../apps/hub-dashboard/src/app/api/proxy/[...path]/route.ts) — no changes required.

**WebSocket cannot go through the Next.js HTTP proxy.** Dashboard connects directly to the hub's WS endpoint. URL is derived the same way `/ws/terminal` already does (environment variable + `window.location`). Phase 1 implementation will align with whatever pattern terminal uses.

### 4.7 Phase 1 dashboard "done" criteria

- [ ] Desktop tab appears for online devices and starts a session on click
- [ ] Debug panel updates in real time with fps / bytes / latency / resolution
- [ ] Stop button cleanly ends the session and releases resources
- [ ] 10-minute manual session does not leak memory (verify: Chrome DevTools heap snapshot diff)
- [ ] Session end from device side (e.g., daemon killed) shows `ended` state, not `error`
- [ ] Page navigation / tab switch terminates the session (cleanup `useEffect`)
- [ ] Vitest unit tests cover the `use-desktop-stream` state machine transitions and frame parsing

## Section 5 — Operations, Testing, Multi-Phase Roadmap

### 5.1 Audit logging

Every desktop session writes audit log entries. Fields:

| Field | Value |
|---|---|
| `event` | `desktop.capture.start` / `desktop.capture.end` / `desktop.capture.rejected` |
| `caller_uid` | Dashboard user who initiated the session |
| `device_id` | Target device |
| `job_id` | Associated job |
| `config` | JSON: `{fps, jpeg_quality, display_index}` |
| `outcome` | `started` / `completed` / `cancelled` / `failed` |
| `duration_ms` | Session length (set at end) |
| `frame_count` | Total frames streamed (set at end) |
| `total_bytes` | Total bytes streamed (set at end) |
| `error` | Failure reason (set on failure) |

Desktop capture is the most sensitive capability in aHand (the operator can observe everything on the device's screen). Audit records `frame_count` and `total_bytes` in addition to the standard job audit so post-facto review can answer "who watched which device for how long, with how much data transferred."

### 5.2 Testing strategy

Per the 100% coverage requirement in the root `CLAUDE.md`.

**Daemon unit tests:**
- [ ] `run_desktop_capture` exits ≤100ms after cancel signal (with fake `EnvelopeSink`)
- [ ] Three consecutive frame errors terminate the session
- [ ] Defaults: fps=0 → 10, jpeg_quality=0 → 70
- [ ] xcap init failure returns a clear error string

**Daemon integration tests:**
- [ ] Full lifecycle: JobRequest → N frames → CancelJob → JobFinished
- [ ] Policy default-deny: `allow_desktop_capture=false` → JobRejected
- [ ] Policy opt-in: `allow_desktop_capture=true` → session runs

**Hub unit tests:**
- [ ] `DesktopFrameBus.publish/subscribe/close` semantics
- [ ] Slow subscriber receives `Lagged(n)` after channel saturation; fast subscribers unaffected
- [ ] Subscribe-before-first-frame works
- [ ] `close` on a non-existent session is a no-op (doesn't panic)

**Hub integration tests:**
- [ ] `handle_device_frame` routes `DesktopFrame` payloads to the bus
- [ ] `/api/desktop/token` create/validate/single-use lifecycle
- [ ] `/ws/desktop` delivers text-metadata + binary-JPEG paired frames
- [ ] Concurrent same-device desktop session creation returns 409 Conflict
- [ ] Session end propagates `{"type":"ended"}` and closes WS

**Dashboard unit tests:**
- [ ] `use-desktop-stream` state machine transitions: idle → requesting-token → connecting → streaming → ended
- [ ] Error paths: token failure → error; WS drop → error
- [ ] Frame parsing: out-of-order text/binary pairs logged as warn, not crash
- [ ] ImageBitmap lifecycle: post-`stop()` no pending bitmaps retained

**End-to-end (manual, Phase 1 not automated):**
- [ ] On macOS, ahandd running + hub-dashboard, click Start → see own screen
- [ ] 10-minute continuous session, Chrome memory stable (heap snapshot)
- [ ] Kill daemon mid-session → dashboard shows ended/error with clear cause

### 5.3 Multi-phase roadmap

All items in each phase are tracked in [docs/remote-control-roadmap.md](../../remote-control-roadmap.md) Section 4 as checkboxes.

| Phase | OS | Video | Input | Encoding | Milestone |
|---|---|---|---|---|---|
| **Phase 1** | macOS | primary display, view-only | — | JPEG @ 10 fps | End-to-end plumbing validated. **Current target.** |
| **Phase 1.5** | macOS | primary display | mouse (normalized [0,1] coords) + basic keyboard (ASCII, modifiers) | JPEG @ 10-15 fps | Add `DesktopInputEvent`, `enigo` injection |
| **Phase 1.9** | macOS | multi-monitor UI | full mouse/keyboard | JPEG | `DesktopDisplayInfo` query + picker UI |
| **Phase 2** | macOS + Linux X11 + Windows | multi-monitor | full | **libvpx VP9 + WebCodecs decode** | Production encoding, 30 fps, adaptive bitrate (basic) |
| **Phase 2W** | macOS + Linux X11 + Windows | **window-scoped capture** (generic) | full, scoped to window | VP9 | OS-level window capture for non-browser apps (browsers go via CDP path in roadmap Section 2) |
| **Phase 2.5** | + Linux Wayland (portal / uinput) | multi-monitor | full | VP9 / H.264 hw encode | Wayland support; multi-observer broadcast |
| **Phase 3** | all platforms | multi-monitor, clipboard, file drag-n-drop | full + IME + layout maps | adaptive QoS (RustDesk `video_qos.rs` reference), hardware encoding | Feature parity with production remote desktop tools |

### 5.4 Phase 1 "Done" master checklist

**Protocol layer**
- [ ] `proto/ahand/v1/envelope.proto`: `DesktopCaptureConfig` + `DesktopFrame` added
- [ ] `JobRequest.desktop_capture` (tag 8)
- [ ] `Envelope.desktop_frame` oneof variant (tag 31)
- [ ] `PolicyState.allow_desktop_capture` (tag 6)
- [ ] `PolicyUpdate.set_allow_desktop_capture` (tag 10)
- [ ] `cargo build -p ahand-protocol` passes; prost codegen correct

**Daemon layer**
- [ ] New `crates/ahandd/src/desktop.rs` module
- [ ] `Cargo.toml` adds `xcap = "0.7"`, `image = "0.25"`
- [ ] `ahand_client.rs::spawn_job` gains desktop branch
- [ ] Policy integration: default deny, config-flag opt-in
- [ ] Unit + integration tests pass

**Hub layer**
- [ ] New `crates/ahand-hub/src/desktop_bus.rs` module
- [ ] New `crates/ahand-hub/src/http/desktop.rs` module
- [ ] `handle_device_frame` routes `DesktopFrame`
- [ ] `/api/jobs` rejects concurrent same-device desktop session (409)
- [ ] `/api/desktop/token` endpoint
- [ ] `/ws/desktop` endpoint
- [ ] Jobs table / store gets `desktop_capture_config` JSONB column + migration
- [ ] Audit logging records the extra `frame_count` / `total_bytes` fields
- [ ] Unit + integration tests pass

**Dashboard layer**
- [ ] `components/device-desktop.tsx`
- [ ] `components/device-desktop-canvas.tsx`
- [ ] `components/device-desktop-debug.tsx`
- [ ] `hooks/use-desktop-stream.ts`
- [ ] `device-tabs.tsx` gains the Desktop tab
- [ ] Vitest tests on the stream hook state machine
- [ ] Manual 10-minute memory leak check passes

**Docs layer**
- [ ] `crates/ahandd/README.md` (or equivalent config doc) gains macOS Screen Recording section
- [ ] This spec (`2026-04-12-remote-desktop-research.md`) status updated to "Phase 1 implemented"
- [ ] `docs/remote-control-roadmap.md` Section 4 Phase 1 checkboxes ticked

**End-to-end validation**
- [ ] On macOS: full stack, click Start, see the screen
- [ ] Stop works; re-Start works
- [ ] Debug panel values are plausible (fps near target, latency < 500ms on localhost)
- [ ] Audit log contains `desktop.capture.start` + `desktop.capture.end` with config + stats

### 5.5 Phase 1 known risks

1. **macOS Screen Recording permission first-run UX** — user must manually grant access in System Settings, may need to restart ahandd. Cannot be eliminated in Phase 1; documented in daemon README.
2. **`xcap` crate compatibility with current macOS** — macOS screen capture APIs change frequently. Phase 1 implementation will smoke-test xcap on the current macOS release and bump the version or switch crate if needed.
3. **Retina-resolution performance** — 2560×1600 or 3840×2160 JPEGs at quality 70 can be 150-300 KB each; at 10 fps that's 1.5-3 MB/s. If this causes stalls or encoding lag on the daemon, temporarily drop target fps to 5 and profile. Not a blocker; Phase 2's VP9 path solves this permanently.

### 5.6 Phase 1 non-goals (tracked to phases in roadmap)

Every item below is a checkbox in [docs/remote-control-roadmap.md](../../remote-control-roadmap.md) Section 4, assigned to a later phase:

| Non-goal | Target phase |
|---|---|
| Input injection (mouse, keyboard) | 1.5 |
| Multi-monitor picker UI | 1.9 |
| Linux X11 support | 2 |
| Windows support | 2 |
| VP9 / H.264 / AV1 encoding | 2 |
| WebCodecs `VideoDecoder` integration | 2 |
| Automatic WS reconnection | 2 |
| Window-scoped capture (non-browser) | 2W |
| Browser window live view | roadmap Section 2 (CDP) |
| Linux Wayland support | 2.5 |
| Multi-observer broadcast | 2.5 |
| Adaptive QoS (fps/bitrate feedback loop) | 3 |
| Clipboard sync | 3 |
| File drag-and-drop | 3 |
| Video recording of sessions | 3 |
| IME / keyboard layout maps | 3 |
| Hardware encoding (NVENC / VAAPI / VideoToolbox) | 3 |

## References

- [Remote Control Roadmap](../../remote-control-roadmap.md) — Section 4 "Remote Desktop / Screen Control" (phase checkboxes live there)
- [Device Exec Terminal Design](2026-04-12-device-exec-terminal-design.md) — the one-time token + WebSocket gateway pattern we mirror
- [Interactive Terminal Plan](../plans/2026-04-12-interactive-terminal.md) — reference for the PTY / bidirectional transport we model after
- [RustDesk source](https://github.com/rustdesk/rustdesk) — reference for per-platform capture, codec tuning, and input injection (we borrow ideas, not architecture)
- `xcap` crate — [https://crates.io/crates/xcap](https://crates.io/crates/xcap)



