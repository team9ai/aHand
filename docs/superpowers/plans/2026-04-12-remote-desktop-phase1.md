# Remote Desktop Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a macOS view-only remote desktop prototype for aHand: an operator opens the hub-dashboard, picks a device, clicks "Start" on a new Desktop tab, and watches the device's primary screen in real time over a JPEG + WebSocket stream.

**Architecture:** A desktop session is a new kind of job — `JobRequest.desktop_capture` is a config sub-message. Control plane (create / cancel / finish) reuses the existing job lifecycle, audit, and policy. Data plane adds a dedicated `DesktopFrame` envelope payload, routed from `device_gateway` through a new live-only `DesktopFrameBus` (no history, no persistence — slow subscribers drop frames) to a new `/ws/desktop` gateway that forwards paired text-metadata + binary-JPEG WebSocket frames to the dashboard. The dashboard renders with `createImageBitmap` + `drawImage` on a `<canvas>`, and always shows a permanent debug panel (fps / bytes / latency / resolution).

**Tech Stack:** Rust (daemon + hub, `xcap 0.7`, `image 0.25`, `tokio::sync::broadcast`, `dashmap`, `axum`, `prost` protobuf), TypeScript/React (hub-dashboard, Next.js 16, `createImageBitmap`, `<canvas>`, `vitest`), Postgres (JSONB column migration).

**Design spec:** [docs/superpowers/specs/2026-04-12-remote-desktop-research.md](../specs/2026-04-12-remote-desktop-research.md)

---

## File Structure

### Created files

| Path | Responsibility |
|---|---|
| `crates/ahandd/src/desktop.rs` | Daemon: desktop capture loop. `run_desktop_capture(device_id, job_id, config, tx, cancel_rx)` drives `xcap` → JPEG → `DesktopFrame` envelope. Pure function, no runtime globals. |
| `crates/ahand-hub/src/desktop_bus.rs` | Hub: per-session live-only broadcast channel registry. `DesktopFrameBus::{new, publish, subscribe, close}`. |
| `crates/ahand-hub/src/http/desktop.rs` | Hub: `/api/desktop/token` (one-time token issuance) and `/ws/desktop` (WebSocket gateway bridging dashboard ↔ desktop bus). |
| `crates/ahand-hub-store/migrations/0002_add_desktop_capture_config.sql` | DB migration: adds `desktop_capture_config JSONB` column to `jobs`. |
| `apps/hub-dashboard/src/hooks/use-desktop-stream.ts` | Dashboard: WebSocket hook + state machine + frame parsing + EMA stats. |
| `apps/hub-dashboard/src/components/device-desktop.tsx` | Dashboard: top-level Desktop tab component. Wires hook, canvas, and debug panel. Owns session lifecycle (start / stop / cleanup). |
| `apps/hub-dashboard/src/components/device-desktop-canvas.tsx` | Dashboard: pure canvas renderer. Accepts frames via ref callback, handles `bitmap.close()` lifecycle. |
| `apps/hub-dashboard/src/components/device-desktop-debug.tsx` | Dashboard: compact one-row debug panel with expandable details. |
| `apps/hub-dashboard/tests/desktop-stream.test.ts` | Vitest unit tests for `use-desktop-stream` state machine. |
| `crates/ahandd/tests/desktop_capture.rs` | Daemon integration test: JobRequest → frames → Cancel → JobFinished. |
| `crates/ahand-hub/tests/desktop_flow.rs` | Hub integration test: full WS flow, token single-use, concurrency 409. |

### Modified files

| Path | Change |
|---|---|
| `proto/ahand/v1/envelope.proto` | Add `DesktopCaptureConfig`, `DesktopFrame`, `DesktopCaptureAllowUpdate` messages. Extend `JobRequest` with `desktop_capture` field. Extend `Envelope.payload` oneof with `desktop_frame`. Extend `PolicyState` with `allow_desktop_capture`. Extend `PolicyUpdate` with `set_allow_desktop_capture`. |
| `crates/ahandd/Cargo.toml` | Add `xcap = "0.7"`. (Verify `image` presence in workspace; add if absent.) |
| `crates/ahandd/src/ahand_client.rs` | `spawn_job` gains a third branch ahead of `interactive` check, routing desktop jobs to `run_desktop_capture`. |
| `crates/ahandd/src/config.rs` | `PolicyConfig` gains `allow_desktop_capture: bool` (default false, `#[serde(default)]`). |
| `crates/ahandd/src/policy.rs` | `Policy::allow_desktop_capture()` accessor. |
| `crates/ahand-hub/src/lib.rs` (or equivalent module root) | Register `desktop_bus` and `http::desktop` modules. |
| `crates/ahand-hub/src/http/mod.rs` | Route registration for `/api/desktop/token` and `/ws/desktop`. |
| `crates/ahand-hub/src/http/jobs.rs` | `handle_device_frame` adds `DesktopFrame` match arm → routes to `DesktopFrameBus::publish`. `JobFinished` arm calls `desktop_bus.close` for desktop jobs. POST `/api/jobs` rejects concurrent same-device desktop sessions (409). Jobs constructor accepts `desktop_capture_config`. |
| `crates/ahand-hub-store/src/job_store.rs` (+ `postgres.rs`) | Persist `desktop_capture_config` JSONB on insert; hydrate on read. |
| `crates/ahand-hub-store/src/audit_store.rs` | New event constants: `desktop.capture.start`, `desktop.capture.end`, `desktop.capture.rejected`. |
| `apps/hub-dashboard/src/components/device-tabs.tsx` | Add `"desktop"` to tab union, render `<DeviceDesktop>` when selected. |
| `crates/ahandd/README.md` | "macOS Screen Recording Permission" section. |
| `docs/superpowers/specs/2026-04-12-remote-desktop-research.md` | Flip `Status:` to "Phase 1 implemented" at end. |
| `docs/remote-control-roadmap.md` | Tick Phase 1 checkboxes in Section 4. |

---

## Task 0: Protocol — protobuf additions

**Goal:** Add all new protobuf messages and fields for Phase 1; verify `ahand-protocol` builds and `prost` codegen is correct. No consumers of these types yet.

**Files:**
- Modify: `proto/ahand/v1/envelope.proto`

**Acceptance Criteria:**
- [ ] `DesktopCaptureConfig`, `DesktopFrame`, `DesktopCaptureAllowUpdate` messages present
- [ ] `JobRequest.desktop_capture` field at tag 8
- [ ] `Envelope.payload` oneof gains `desktop_frame` variant at tag 31
- [ ] `PolicyState.allow_desktop_capture` at tag 6
- [ ] `PolicyUpdate.set_allow_desktop_capture` at tag 10 using `DesktopCaptureAllowUpdate`
- [ ] `cargo build -p ahand-protocol` passes
- [ ] `cargo test -p ahand-protocol` passes (existing tests should still pass; the new types are only schema changes)

**Verify:** `cargo build -p ahand-protocol && cargo test -p ahand-protocol` → compiles cleanly, no errors.

**Steps:**

- [ ] **Step 1: Read the existing envelope.proto** to confirm tag numbers and structure.

Run: Read `proto/ahand/v1/envelope.proto` end-to-end.
Expected: Envelope.payload highest tag is 30 (`terminal_resize`), `JobRequest` has 7 fields, `PolicyState` has 5 fields, `PolicyUpdate` has 9 fields. (These are the tag slots tasks 0.2–0.6 will extend.)

- [ ] **Step 2: Add `DesktopCaptureConfig` + `DesktopFrame` + `DesktopCaptureAllowUpdate`** near the bottom of the file, just before the closing of the file (after the PTY / interactive section added by the earlier terminal feature).

```protobuf
// ── Desktop Capture ─────────────────────────────────────────────

// DesktopCaptureConfig - embedded in JobRequest.desktop_capture when the job
// is a desktop session rather than a command execution.
message DesktopCaptureConfig {
  uint32 fps           = 1;  // target frame rate; 0 = default (10)
  uint32 jpeg_quality  = 2;  // 1-100; 0 = default (70)
  uint32 display_index = 3;  // 0 = primary; reserved for Phase 1.9 multi-monitor UI
}

// DesktopFrame - streamed daemon → hub during an active capture session.
// Bound to an existing desktop-capture job via job_id.
message DesktopFrame {
  string job_id         = 1;
  uint64 frame_id       = 2;  // monotonic, 0-based
  uint32 width          = 3;
  uint32 height         = 4;
  string mime           = 5;  // "image/jpeg" in Phase 1
  bytes  data           = 6;  // encoded frame payload
  uint64 captured_at_ms = 7;  // daemon wall-clock, for latency debug panel
}

// DesktopCaptureAllowUpdate - tri-state for PolicyUpdate.set_allow_desktop_capture.
// Uses UNCHANGED = 0 sentinel to match the existing PolicyUpdate convention.
enum DesktopCaptureAllowUpdate {
  DESKTOP_CAPTURE_ALLOW_UNCHANGED = 0;  // default; no change
  DESKTOP_CAPTURE_ALLOW_DENY      = 1;
  DESKTOP_CAPTURE_ALLOW_GRANT     = 2;
}
```

- [ ] **Step 3: Extend `JobRequest`** with `desktop_capture` at tag 8.

Modify the `JobRequest` message:
```protobuf
message JobRequest {
  string job_id = 1;
  string tool   = 2;
  repeated string args = 3;
  string cwd    = 4;
  map<string, string> env = 5;
  uint64 timeout_ms = 6;
  bool   interactive = 7;
  // If present, this is a desktop capture session. When set, the daemon
  // ignores tool/args/cwd/env/interactive and spawns a capture loop instead.
  DesktopCaptureConfig desktop_capture = 8;
}
```

- [ ] **Step 4: Extend `Envelope.payload` oneof** with the new variant at tag 31.

```protobuf
oneof payload {
  // ...existing 20 variants at tags 9-30...
  StdinChunk     stdin_chunk     = 29;
  TerminalResize terminal_resize = 30;
  DesktopFrame   desktop_frame   = 31;
}
```

- [ ] **Step 5: Extend `PolicyState`** with `allow_desktop_capture` at tag 6.

```protobuf
message PolicyState {
  repeated string allowed_tools = 1;
  repeated string denied_tools  = 2;
  repeated string denied_paths  = 3;
  repeated string allowed_domains = 4;
  uint64 approval_timeout_secs = 5;
  bool   allow_desktop_capture = 6;
}
```

- [ ] **Step 6: Extend `PolicyUpdate`** with `set_allow_desktop_capture` at tag 10.

```protobuf
message PolicyUpdate {
  repeated string add_allowed_tools      = 1;
  repeated string remove_allowed_tools   = 2;
  repeated string add_denied_tools       = 3;
  repeated string remove_denied_tools    = 4;
  repeated string add_allowed_domains    = 5;
  repeated string remove_allowed_domains = 6;
  repeated string add_denied_paths       = 7;
  repeated string remove_denied_paths    = 8;
  uint64 approval_timeout_secs = 9;
  DesktopCaptureAllowUpdate set_allow_desktop_capture = 10;
}
```

- [ ] **Step 7: Build and test the protocol crate.**

Run: `cargo build -p ahand-protocol && cargo test -p ahand-protocol`
Expected: both pass. If prost complains about tag reuse or the build.rs regeneration misses the new types, fix before moving on.

- [ ] **Step 8: Commit.**

```bash
git add proto/ahand/v1/envelope.proto
git commit -m "feat(protocol): add DesktopCaptureConfig, DesktopFrame, and policy extensions for Phase 1 remote desktop"
```

---

## Task 1: Daemon — `desktop.rs` capture module

**Goal:** Implement `run_desktop_capture` as a standalone module, TDD-style with fake `EnvelopeSink`. Not wired into `spawn_job` yet — this task is just the capture loop + its tests.

**Files:**
- Create: `crates/ahandd/src/desktop.rs`
- Modify: `crates/ahandd/src/lib.rs` (or `main.rs` — wherever modules are declared) — add `pub mod desktop;`
- Modify: `crates/ahandd/Cargo.toml` — add `xcap = "0.7"`; ensure `image = "0.25"` is available (check workspace first)

**Acceptance Criteria:**
- [ ] `run_desktop_capture` exists with the signature shown in Step 3
- [ ] Cancel signal causes exit within 100 ms (unit test verifies)
- [ ] Default config values applied when `fps=0`, `jpeg_quality=0`
- [ ] Three consecutive capture errors terminate the loop with `(1, "capture loop failed: ...")`
- [ ] JPEG encoding runs in `spawn_blocking` (grep the code to confirm)
- [ ] All unit tests pass: `cargo test -p ahandd desktop::`

**Verify:** `cargo test -p ahandd desktop::` → all tests pass.

**Steps:**

- [ ] **Step 1: Add `xcap` dependency to `crates/ahandd/Cargo.toml`.**

Check `image` crate presence first:
```bash
cargo tree -p ahandd --prefix none 2>/dev/null | grep -E '^image v'
```
If `image` is not present at 0.25+ in the workspace, add it explicitly alongside `xcap`.

Add under `[dependencies]`:
```toml
xcap  = "0.7"
image = "0.25"
```

Run `cargo check -p ahandd` to confirm the deps resolve.

- [ ] **Step 2: Declare the module.**

In `crates/ahandd/src/lib.rs` (or wherever sibling modules like `executor`, `policy`, `session` are declared), add:
```rust
pub mod desktop;
```

- [ ] **Step 3: Write the failing unit tests.** Create `crates/ahandd/src/desktop.rs` with a test module referencing `run_desktop_capture` (which doesn't exist yet — the tests will fail to compile, which is the red phase).

```rust
use crate::executor::EnvelopeSink;
use ahand_protocol::{DesktopCaptureConfig, Envelope};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Fake sink that records every envelope it receives.
#[derive(Clone, Default)]
struct CapturedSink {
    frames: Arc<Mutex<Vec<Envelope>>>,
}

impl EnvelopeSink for CapturedSink {
    fn send(&self, envelope: Envelope) -> Result<(), ()> {
        self.frames.lock().unwrap().push(envelope);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    // Helper: real xcap backends require a display. These tests run on CI
    // without one, so we swap the capture backend for an injectable trait.
    // See `CaptureBackend` in the implementation below.

    #[tokio::test]
    async fn cancel_exits_within_100ms() {
        let sink = CapturedSink::default();
        let (cancel_tx, cancel_rx) = mpsc::channel(1);
        let cfg = DesktopCaptureConfig { fps: 30, jpeg_quality: 50, display_index: 0 };

        // Use a fake backend that always returns a 16x16 white frame
        let backend = crate::desktop::FakeBackend::white(16, 16);
        let task = tokio::spawn(crate::desktop::run_desktop_capture_with_backend(
            "dev-1".into(), "job-1".into(), cfg, sink.clone(), cancel_rx, Box::new(backend),
        ));

        // Let it capture for 200 ms (should get ~6 frames at 30 fps)
        tokio::time::sleep(Duration::from_millis(200)).await;
        let start = Instant::now();
        cancel_tx.send(()).await.unwrap();
        let (code, err) = task.await.unwrap();
        assert!(start.elapsed() < Duration::from_millis(100), "cancel took too long: {:?}", start.elapsed());
        assert_eq!(code, 0);
        assert_eq!(err, "");
        assert!(!sink.frames.lock().unwrap().is_empty(), "should have captured at least one frame");
    }

    #[tokio::test]
    async fn applies_defaults_when_zero() {
        // With fps=0 we expect the default (10 fps). We'll measure inter-frame timing.
        let sink = CapturedSink::default();
        let (cancel_tx, cancel_rx) = mpsc::channel(1);
        let cfg = DesktopCaptureConfig { fps: 0, jpeg_quality: 0, display_index: 0 };
        let backend = crate::desktop::FakeBackend::white(8, 8);
        let task = tokio::spawn(crate::desktop::run_desktop_capture_with_backend(
            "dev-1".into(), "job-1".into(), cfg, sink.clone(), cancel_rx, Box::new(backend),
        ));
        tokio::time::sleep(Duration::from_millis(350)).await;  // enough for ~3 frames at 10 fps
        cancel_tx.send(()).await.unwrap();
        let _ = task.await;
        let n = sink.frames.lock().unwrap().len();
        assert!((2..=5).contains(&n), "expected ~3 frames at default 10 fps, got {}", n);
    }

    #[tokio::test]
    async fn three_consecutive_errors_terminate_session() {
        let sink = CapturedSink::default();
        let (_cancel_tx, cancel_rx) = mpsc::channel(1);
        let cfg = DesktopCaptureConfig { fps: 50, jpeg_quality: 70, display_index: 0 };
        let backend = crate::desktop::FakeBackend::always_fails();
        let (code, err) = crate::desktop::run_desktop_capture_with_backend(
            "dev-1".into(), "job-1".into(), cfg, sink.clone(), cancel_rx, Box::new(backend),
        ).await;
        assert_eq!(code, 1);
        assert!(err.starts_with("capture loop failed"), "unexpected error: {}", err);
        assert!(sink.frames.lock().unwrap().is_empty());
    }
}
```

- [ ] **Step 4: Run tests, confirm they fail to compile.**

Run: `cargo test -p ahandd desktop::`
Expected: compile errors — `run_desktop_capture_with_backend`, `FakeBackend` do not exist yet.

- [ ] **Step 5: Write the implementation.** Replace the `desktop.rs` file contents with the full module:

```rust
use crate::executor::EnvelopeSink;
use ahand_protocol::{DesktopCaptureConfig, DesktopFrame, Envelope, envelope};
use image::{ImageBuffer, Rgba, codecs::jpeg::JpegEncoder};
use std::io::Cursor;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

const DEFAULT_FPS: u32 = 10;
const DEFAULT_QUALITY: u32 = 70;
const MAX_CONSECUTIVE_ERRORS: u32 = 3;

/// A single captured frame, decoupled from the capture backend.
pub struct RawFrame {
    pub width: u32,
    pub height: u32,
    /// BGRA / RGBA pixel buffer, 8-bit per channel, row-major.
    pub pixels: Vec<u8>,
}

/// Pluggable capture backend so unit tests can run without a display.
pub trait CaptureBackend: Send {
    fn capture(&mut self) -> Result<RawFrame, String>;
}

/// Production xcap-based backend for the primary display.
pub struct XcapBackend {
    monitor: xcap::Monitor,
}

impl XcapBackend {
    pub fn primary() -> Result<Self, String> {
        let monitors = xcap::Monitor::all().map_err(|e| format!("xcap init failed: {e}"))?;
        let primary = monitors
            .into_iter()
            .find(|m| m.is_primary().unwrap_or(false))
            .ok_or_else(|| "no primary monitor".to_string())?;
        Ok(XcapBackend { monitor: primary })
    }
}

impl CaptureBackend for XcapBackend {
    fn capture(&mut self) -> Result<RawFrame, String> {
        let img = self.monitor.capture_image().map_err(|e| format!("capture failed: {e}"))?;
        let (w, h) = img.dimensions();
        Ok(RawFrame { width: w, height: h, pixels: img.into_raw() })
    }
}

/// Run a desktop capture session. See the design spec for contract.
pub async fn run_desktop_capture<T: EnvelopeSink>(
    device_id: String,
    job_id: String,
    config: DesktopCaptureConfig,
    tx: T,
    cancel_rx: mpsc::Receiver<()>,
) -> (i32, String) {
    let backend: Box<dyn CaptureBackend> = match XcapBackend::primary() {
        Ok(b) => Box::new(b),
        Err(e) => return (1, format!("screen recording permission denied or capture init failed: {e}. grant access in System Settings → Privacy & Security → Screen Recording, then restart ahandd")),
    };
    run_desktop_capture_with_backend(device_id, job_id, config, tx, cancel_rx, backend).await
}

/// Backend-injectable variant used by unit tests (and by `run_desktop_capture`).
pub async fn run_desktop_capture_with_backend<T: EnvelopeSink>(
    device_id: String,
    job_id: String,
    config: DesktopCaptureConfig,
    tx: T,
    mut cancel_rx: mpsc::Receiver<()>,
    mut backend: Box<dyn CaptureBackend>,
) -> (i32, String) {
    let fps = if config.fps == 0 { DEFAULT_FPS } else { config.fps.min(60) };
    let quality = if config.jpeg_quality == 0 { DEFAULT_QUALITY } else { config.jpeg_quality.min(100) } as u8;
    let period = Duration::from_millis((1000u64.max(1)) / fps as u64);

    info!(job_id = %job_id, fps, quality, "desktop capture loop started");
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut frame_id: u64 = 0;
    let mut consecutive_errors: u32 = 0;

    loop {
        tokio::select! {
            biased;
            _ = cancel_rx.recv() => {
                info!(job_id = %job_id, frame_id, "desktop capture cancelled");
                return (0, String::new());
            }
            _ = ticker.tick() => {
                match backend.capture() {
                    Ok(raw) => {
                        // JPEG encode off the runtime thread
                        let encoded = tokio::task::spawn_blocking(move || encode_jpeg(&raw, quality)).await;
                        match encoded {
                            Ok(Ok((jpeg, w, h))) => {
                                consecutive_errors = 0;
                                let now_ms = crate::desktop::now_ms();
                                let env = make_frame_envelope(&device_id, &job_id, frame_id, w, h, jpeg, now_ms);
                                if tx.send(env).is_err() {
                                    debug!(job_id = %job_id, "envelope sink closed, ending desktop capture");
                                    return (0, String::new());
                                }
                                frame_id += 1;
                            }
                            Ok(Err(e)) => {
                                warn!(job_id = %job_id, error = %e, "jpeg encode failed");
                                consecutive_errors += 1;
                            }
                            Err(join_err) => {
                                warn!(job_id = %job_id, error = %join_err, "encode task join failed");
                                consecutive_errors += 1;
                            }
                        }
                    }
                    Err(e) => {
                        warn!(job_id = %job_id, error = %e, "capture failed");
                        consecutive_errors += 1;
                    }
                }
                if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                    return (1, format!("capture loop failed: {} consecutive errors", consecutive_errors));
                }
            }
        }
    }
}

fn encode_jpeg(raw: &RawFrame, quality: u8) -> Result<(Vec<u8>, u32, u32), String> {
    let buf: ImageBuffer<Rgba<u8>, _> = ImageBuffer::from_raw(raw.width, raw.height, raw.pixels.clone())
        .ok_or_else(|| "pixel buffer size mismatch".to_string())?;
    let mut out = Vec::with_capacity(raw.pixels.len() / 4);
    let mut encoder = JpegEncoder::new_with_quality(Cursor::new(&mut out), quality);
    encoder.encode_image(&buf).map_err(|e| format!("jpeg encode: {e}"))?;
    Ok((out, raw.width, raw.height))
}

fn make_frame_envelope(device_id: &str, job_id: &str, frame_id: u64, width: u32, height: u32, data: Vec<u8>, captured_at_ms: u64) -> Envelope {
    Envelope {
        device_id: device_id.to_string(),
        trace_id: String::new(),
        msg_id: format!("desktop-{}-{}", job_id, frame_id),
        seq: 0, ack: 0, ts_ms: captured_at_ms,
        payload: Some(envelope::Payload::DesktopFrame(DesktopFrame {
            job_id: job_id.to_string(),
            frame_id,
            width,
            height,
            mime: "image/jpeg".to_string(),
            data,
            captured_at_ms,
        })),
    }
}

pub(crate) fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ── Test helpers ─────────────────────────────────────────────────

#[cfg(test)]
pub struct FakeBackend {
    mode: FakeMode,
}

#[cfg(test)]
enum FakeMode {
    White { width: u32, height: u32 },
    AlwaysFails,
}

#[cfg(test)]
impl FakeBackend {
    pub fn white(width: u32, height: u32) -> Self { Self { mode: FakeMode::White { width, height } } }
    pub fn always_fails() -> Self { Self { mode: FakeMode::AlwaysFails } }
}

#[cfg(test)]
impl CaptureBackend for FakeBackend {
    fn capture(&mut self) -> Result<RawFrame, String> {
        match &self.mode {
            FakeMode::White { width, height } => {
                let pixels = vec![255u8; (*width as usize) * (*height as usize) * 4];
                Ok(RawFrame { width: *width, height: *height, pixels })
            }
            FakeMode::AlwaysFails => Err("synthetic failure".to_string()),
        }
    }
}

// Keep the #[cfg(test)] mod tests at the bottom (written in Step 3 above).
```

(Re-include the `#[cfg(test)] mod tests { ... }` block from Step 3 at the bottom of the file — do not duplicate it.)

- [ ] **Step 6: Run tests again, confirm they pass.**

Run: `cargo test -p ahandd desktop::`
Expected: all three tests pass. Watch for xcap linker issues on the build host (on macOS it needs `-framework CoreGraphics` etc.; xcap handles this).

- [ ] **Step 7: Commit.**

```bash
git add crates/ahandd/Cargo.toml crates/ahandd/src/desktop.rs crates/ahandd/src/lib.rs
git commit -m "feat(daemon): add desktop.rs capture module with xcap + jpeg encoding"
```

---

## Task 2: Daemon — wire desktop capture into `spawn_job` + policy gate

**Goal:** Route `JobRequest` with `desktop_capture` through the new desktop branch, enforce the `allow_desktop_capture` policy, and add an integration test covering the full daemon-side lifecycle.

**Files:**
- Modify: `crates/ahandd/src/ahand_client.rs` — `spawn_job` dispatcher
- Modify: `crates/ahandd/src/config.rs` — `PolicyConfig` gains `allow_desktop_capture`
- Modify: `crates/ahandd/src/policy.rs` — accessor for the new flag
- Create: `crates/ahandd/tests/desktop_capture.rs` — integration test

**Acceptance Criteria:**
- [ ] `PolicyConfig::allow_desktop_capture` defaults to `false` with `#[serde(default)]`
- [ ] `Policy::allow_desktop_capture()` returns the config value
- [ ] `spawn_job` branches on `req.desktop_capture.is_some()` BEFORE the existing `interactive` check
- [ ] Desktop job with policy disabled → `JobRejected` envelope with reason "desktop capture not permitted by policy"
- [ ] Desktop job with policy enabled → frames stream, cancel works, `JobFinished` emitted
- [ ] Integration test exercises both paths

**Verify:** `cargo test -p ahandd --test desktop_capture` → all tests pass.

**Steps:**

- [ ] **Step 1: Read existing `policy.rs` + `config.rs`** to understand the struct shapes you're extending.

Run: Read `crates/ahandd/src/policy.rs` and `crates/ahandd/src/config.rs` fully. Note where the existing `allowed_tools` / `denied_tools` fields live — the new `allow_desktop_capture` field goes in the same struct.

- [ ] **Step 2: Extend `PolicyConfig`.**

In `crates/ahandd/src/config.rs`, inside `PolicyConfig`:
```rust
pub struct PolicyConfig {
    // ...existing fields...
    #[serde(default)]
    pub allow_desktop_capture: bool,
}
```

Verify the default — with `#[serde(default)]` on the field and `Default for bool` returning `false`, old configs without this key deserialize as `false`.

- [ ] **Step 3: Extend `Policy`** with an accessor.

In `crates/ahandd/src/policy.rs`:
```rust
impl Policy {
    // ...existing methods...
    pub fn allow_desktop_capture(&self) -> bool {
        self.config.allow_desktop_capture
    }
}
```

(If `Policy` stores its config differently, adapt the field access to match. The existing `allowed_tools()` method is the pattern to follow.)

- [ ] **Step 4: Write the failing integration test.** Create `crates/ahandd/tests/desktop_capture.rs`:

```rust
use ahand_protocol::{DesktopCaptureConfig, Envelope, JobRequest, envelope};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

/// Capture all envelopes the daemon tries to send upstream.
#[derive(Clone, Default)]
struct CapturingSink(Arc<Mutex<Vec<Envelope>>>);

impl ahandd::executor::EnvelopeSink for CapturingSink {
    fn send(&self, e: Envelope) -> Result<(), ()> {
        self.0.lock().unwrap().push(e);
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread", start_paused = false)]
async fn policy_denied_returns_job_rejected() {
    // Build a Policy with allow_desktop_capture = false (default)
    let policy = ahandd::policy::Policy::from_config(Default::default());
    let sink = CapturingSink::default();

    // Construct a JobRequest with desktop_capture set
    let req = JobRequest {
        job_id: "job-a".into(),
        desktop_capture: Some(DesktopCaptureConfig { fps: 10, jpeg_quality: 70, display_index: 0 }),
        ..Default::default()
    };

    // Dispatch through the same path spawn_job uses. The simplest way is to
    // invoke the policy check directly — we'll mirror the real spawn_job
    // wiring in Step 5 below.
    let allowed = policy.allow_desktop_capture();
    assert!(!allowed);

    // When spawn_job sees allowed=false, it must emit a JobRejected envelope.
    // Call the helper that spawn_job will call:
    ahandd::ahand_client::emit_job_rejected(&sink, "job-a", "desktop capture not permitted by policy");
    let frames = sink.0.lock().unwrap();
    assert_eq!(frames.len(), 1);
    match &frames[0].payload {
        Some(envelope::Payload::JobRejected(r)) => {
            assert_eq!(r.job_id, "job-a");
            assert!(r.reason.contains("desktop capture not permitted"));
        }
        other => panic!("expected JobRejected, got {:?}", other),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn policy_allowed_runs_desktop_loop_and_cancels() {
    // Build a Policy with allow_desktop_capture = true
    let mut cfg = ahandd::config::PolicyConfig::default();
    cfg.allow_desktop_capture = true;
    let policy = ahandd::policy::Policy::from_config(cfg);
    let sink = CapturingSink::default();

    let (cancel_tx, cancel_rx) = mpsc::channel(1);
    // Bypass the xcap backend with the FakeBackend from the module
    let backend = Box::new(ahandd::desktop::FakeBackend::white(32, 32));
    let cfg = DesktopCaptureConfig { fps: 30, jpeg_quality: 60, display_index: 0 };

    assert!(policy.allow_desktop_capture());
    let task = tokio::spawn(ahandd::desktop::run_desktop_capture_with_backend(
        "dev-1".into(), "job-b".into(), cfg, sink.clone(), cancel_rx, backend
    ));
    tokio::time::sleep(Duration::from_millis(150)).await;
    cancel_tx.send(()).await.unwrap();
    let (code, err) = task.await.unwrap();
    assert_eq!(code, 0);
    assert_eq!(err, "");
    let frames = sink.0.lock().unwrap();
    assert!(!frames.is_empty());
    // Every captured envelope should be a DesktopFrame
    for f in frames.iter() {
        assert!(matches!(&f.payload, Some(envelope::Payload::DesktopFrame(_))));
    }
}
```

Note: `FakeBackend` was declared `#[cfg(test)]` in Task 1. For integration tests it needs to be `pub`-accessible via a feature flag or moved behind `#[cfg(any(test, feature = "test-support"))]`. Use the feature-flag approach: add `test-support = []` to `[features]` in `crates/ahandd/Cargo.toml` and gate `FakeBackend` with `#[cfg(any(test, feature = "test-support"))]`. The integration test activates it via `cargo test --features test-support`.

- [ ] **Step 5: Write the `spawn_job` desktop branch.**

In `crates/ahandd/src/ahand_client.rs`, find `spawn_job` (around line 594). Add the desktop branch **before** the `if interactive` check:

```rust
async fn spawn_job<T>(
    device_id: &str,
    req: ahand_protocol::JobRequest,
    tx: &T,
    registry: &Arc<JobRegistry>,
    store: &Option<Arc<RunStore>>,
    policy: &Arc<crate::policy::Policy>,
) where
    T: crate::executor::EnvelopeSink,
{
    let job_id = req.job_id.clone();
    let tx_clone = (*tx).clone();
    let did = device_id.to_string();
    let reg = Arc::clone(registry);
    let _st = store.clone();

    // --- Desktop capture branch ---
    if let Some(dc_config) = req.desktop_capture.clone() {
        if !policy.allow_desktop_capture() {
            emit_job_rejected(tx, &job_id, "desktop capture not permitted by policy");
            return;
        }
        let (cancel_tx, cancel_rx) = mpsc::channel(1);
        reg.register(job_id.clone(), cancel_tx).await;
        let active = reg.active_count().await;
        tracing::info!(job_id = %job_id, active_jobs = active, kind = "desktop", "desktop capture accepted");
        tokio::spawn(async move {
            let _permit = reg.acquire_permit().await;
            let (exit_code, error) = crate::desktop::run_desktop_capture(
                did, job_id.clone(), dc_config, tx_clone, cancel_rx,
            ).await;
            reg.remove(&job_id).await;
            reg.mark_completed(job_id, exit_code, error).await;
        });
        return;
    }

    // --- Existing interactive / pipe branches below, unchanged ---
    let interactive = req.interactive;
    let (cancel_tx, cancel_rx) = mpsc::channel(1);
    // ... existing code ...
}

/// Emit a JobRejected envelope. Exposed at module level so integration tests
/// can call it directly without wiring up a full spawn_job invocation.
pub fn emit_job_rejected<T: crate::executor::EnvelopeSink>(tx: &T, job_id: &str, reason: &str) {
    let env = ahand_protocol::Envelope {
        payload: Some(ahand_protocol::envelope::Payload::JobRejected(
            ahand_protocol::JobRejected { job_id: job_id.to_string(), reason: reason.to_string() }
        )),
        ..Default::default()
    };
    let _ = tx.send(env);
}
```

You will need to thread `policy: &Arc<Policy>` into `spawn_job` — trace the existing call sites and pass it from the caller. If the caller already has a `Policy` reference available (it should, since existing paths also check policy), reuse that.

- [ ] **Step 6: Run the integration test. Expect it to pass.**

Run: `cargo test -p ahandd --test desktop_capture --features test-support`
Expected: both tests pass.

- [ ] **Step 7: Run the full daemon test suite to ensure no regressions.**

Run: `cargo test -p ahandd`
Expected: all previously-passing tests still pass.

- [ ] **Step 8: Commit.**

```bash
git add crates/ahandd/src/config.rs crates/ahandd/src/policy.rs crates/ahandd/src/ahand_client.rs crates/ahandd/src/desktop.rs crates/ahandd/Cargo.toml crates/ahandd/tests/desktop_capture.rs
git commit -m "feat(daemon): wire desktop capture into spawn_job behind allow_desktop_capture policy"
```

---

## Task 3: Hub — `DesktopFrameBus` module

**Goal:** Add a per-session live-only broadcast channel registry in the hub. No history, no persistence, slow subscribers drop frames. Pure in-memory module with comprehensive unit tests.

**Files:**
- Create: `crates/ahand-hub/src/desktop_bus.rs`
- Modify: `crates/ahand-hub/src/lib.rs` (or module root) — declare `pub mod desktop_bus;`

**Acceptance Criteria:**
- [ ] `DesktopFrameBus::new()`, `publish`, `subscribe`, `close` implemented
- [ ] Channel capacity 4 (frames drop once a subscriber is more than 4 behind)
- [ ] `subscribe` called before `publish` works (entry created lazily)
- [ ] Multiple subscribers receive the same frames via `Arc<DesktopFrame>`
- [ ] Slow subscriber receives `broadcast::error::RecvError::Lagged(n)` once channel saturates; fast subscribers unaffected
- [ ] `close` on a non-existent session is a no-op
- [ ] `close` removes the session so subsequent `publish` creates a new one
- [ ] Unit tests cover all of the above

**Verify:** `cargo test -p ahand-hub desktop_bus::` → all tests pass.

**Steps:**

- [ ] **Step 1: Write the failing unit tests.** Create `crates/ahand-hub/src/desktop_bus.rs`:

```rust
use ahand_protocol::DesktopFrame;
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

// Channel capacity: if a subscriber is more than 4 frames behind, it drops.
const CHANNEL_CAPACITY: usize = 4;

#[derive(Default)]
pub struct DesktopFrameBus {
    sessions: DashMap<String, broadcast::Sender<Arc<DesktopFrame>>>,
}

impl DesktopFrameBus {
    pub fn new() -> Self { Self::default() }

    /// Publish a frame to all subscribers of `job_id`. Returns the number
    /// of receivers at publish time (0 = no one is listening, but the
    /// channel still exists so late subscribers can start receiving
    /// the NEXT frame).
    pub fn publish(&self, job_id: &str, frame: DesktopFrame) -> usize {
        let entry = self.sessions
            .entry(job_id.to_string())
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0);
        let tx = entry.value();
        let frame = Arc::new(frame);
        // `send` returns Err only if there are no receivers — still a valid
        // state, we just return 0 count.
        tx.send(frame).map(|n| n).unwrap_or(0)
    }

    /// Subscribe to a session, creating the channel if it doesn't exist.
    pub fn subscribe(&self, job_id: &str) -> broadcast::Receiver<Arc<DesktopFrame>> {
        let entry = self.sessions
            .entry(job_id.to_string())
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0);
        entry.value().subscribe()
    }

    /// Remove a session entirely. No-op if the session doesn't exist.
    pub fn close(&self, job_id: &str) {
        self.sessions.remove(job_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast::error::TryRecvError;

    fn make_frame(id: u64) -> DesktopFrame {
        DesktopFrame {
            job_id: "job-1".into(),
            frame_id: id,
            width: 16,
            height: 16,
            mime: "image/jpeg".into(),
            data: vec![0u8; 64],
            captured_at_ms: id,
        }
    }

    #[tokio::test]
    async fn publish_without_subscribers_is_ok() {
        let bus = DesktopFrameBus::new();
        let count = bus.publish("job-1", make_frame(0));
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn subscribe_before_first_publish_works() {
        let bus = DesktopFrameBus::new();
        let mut rx = bus.subscribe("job-1");
        bus.publish("job-1", make_frame(0));
        let got = rx.recv().await.expect("should receive");
        assert_eq!(got.frame_id, 0);
    }

    #[tokio::test]
    async fn multiple_subscribers_receive_same_frame() {
        let bus = DesktopFrameBus::new();
        let mut a = bus.subscribe("job-1");
        let mut b = bus.subscribe("job-1");
        bus.publish("job-1", make_frame(5));
        let got_a = a.recv().await.expect("a");
        let got_b = b.recv().await.expect("b");
        assert_eq!(got_a.frame_id, 5);
        assert_eq!(got_b.frame_id, 5);
        // Arc pointer equality: the frame is shared, not copied
        assert!(Arc::ptr_eq(&got_a, &got_b));
    }

    #[tokio::test]
    async fn slow_subscriber_lags_fast_subscriber_unaffected() {
        let bus = DesktopFrameBus::new();
        let mut slow = bus.subscribe("job-1");
        let mut fast = bus.subscribe("job-1");

        // Fill channel beyond capacity (4)
        for i in 0..10 {
            bus.publish("job-1", make_frame(i));
        }

        // Fast subscriber drains immediately
        let mut fast_received = 0u64;
        while let Ok(_) = fast.try_recv() {
            fast_received += 1;
            if fast_received > 20 { break; }  // safety
        }
        // Fast subscriber sees at most CHANNEL_CAPACITY frames before the
        // earliest ones are dropped; that is fine as long as it eventually
        // drains without lag.
        assert!(fast_received >= 1);

        // Slow subscriber sees Lagged
        match slow.try_recv() {
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                assert!(n > 0);
            }
            other => panic!("expected Lagged, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn close_removes_session() {
        let bus = DesktopFrameBus::new();
        bus.publish("job-1", make_frame(0));
        bus.close("job-1");
        // After close, publishing creates a fresh channel
        let count = bus.publish("job-1", make_frame(1));
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn close_nonexistent_is_noop() {
        let bus = DesktopFrameBus::new();
        bus.close("job-404");  // must not panic
    }
}
```

- [ ] **Step 2: Declare the module.**

In `crates/ahand-hub/src/lib.rs` (where sibling modules are declared), add:
```rust
pub mod desktop_bus;
```

- [ ] **Step 3: Check that `dashmap` is already a dependency.**

Run: `cargo tree -p ahand-hub --prefix none 2>/dev/null | grep '^dashmap'`
If absent, add `dashmap = "6"` (or whatever version is already used elsewhere in the workspace) to `crates/ahand-hub/Cargo.toml`.

- [ ] **Step 4: Run tests.**

Run: `cargo test -p ahand-hub desktop_bus::`
Expected: all 6 tests pass.

- [ ] **Step 5: Commit.**

```bash
git add crates/ahand-hub/Cargo.toml crates/ahand-hub/src/desktop_bus.rs crates/ahand-hub/src/lib.rs
git commit -m "feat(hub): add DesktopFrameBus live-only broadcast channel for desktop sessions"
```

---

## Task 4: Hub — `JobRecord.desktop_capture_config` field + `handle_device_frame` routes `DesktopFrame`

**Goal:** Extend the `JobRecord` struct with the new `desktop_capture_config` field (the serde type only — DB/store wiring is Task 5). Wire the `DesktopFrameBus` into the hub's inbound device frame router. Add `DesktopFrameBus::close` to the `JobFinished` arm so session channels are cleaned up.

**Files:**
- Modify: `crates/ahand-hub-store/src/job_store.rs` (or wherever `JobRecord` is defined) — add `DesktopCaptureConfigJson` type and `desktop_capture_config: Option<DesktopCaptureConfigJson>` field + `is_desktop_capture()` helper
- Modify: `crates/ahand-hub-store/src/lib.rs` — export `DesktopCaptureConfigJson`
- Modify: `crates/ahand-hub/src/http/jobs.rs` — extend `handle_device_frame` match + inject bus field into the state struct
- Modify: `crates/ahand-hub/src/http/mod.rs` (or wherever the state struct is built) — construct and share the bus

**Acceptance Criteria:**
- [ ] `DesktopCaptureConfigJson` serde struct exists and round-trips with `ahand_protocol::DesktopCaptureConfig` via `From` impls
- [ ] `JobRecord.desktop_capture_config: Option<DesktopCaptureConfigJson>` present (default `None` in existing constructors)
- [ ] `JobRecord::is_desktop_capture()` returns `true` iff the field is `Some`
- [ ] The state struct (`JobsState` or equivalent) owns an `Arc<DesktopFrameBus>`
- [ ] `handle_device_frame` match arm for `Payload::DesktopFrame` transitions Pending → Running (first frame) and publishes to bus
- [ ] `JobFinished` arm calls `desktop_bus.close(job_id)` for desktop-capture jobs
- [ ] Unknown `job_id` on a `DesktopFrame` returns an error (same as `JobEvent`)
- [ ] Terminal-status jobs silently drop late frames (same as `JobEvent`)
- [ ] Unit test in `handle_device_frame_tests` mod: inject a fake device frame → verify bus subscriber receives it

**Verify:** `cargo test -p ahand-hub http::jobs:: -- handle_device_frame && cargo check --workspace` → passes.

**Steps:**

- [ ] **Step 0: Add `DesktopCaptureConfigJson` and extend `JobRecord`.**

In `crates/ahand-hub-store/src/job_store.rs` (or wherever `JobRecord` lives):

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopCaptureConfigJson {
    pub fps: u32,
    pub jpeg_quality: u32,
    pub display_index: u32,
}

impl From<ahand_protocol::DesktopCaptureConfig> for DesktopCaptureConfigJson {
    fn from(v: ahand_protocol::DesktopCaptureConfig) -> Self {
        Self { fps: v.fps, jpeg_quality: v.jpeg_quality, display_index: v.display_index }
    }
}

impl From<DesktopCaptureConfigJson> for ahand_protocol::DesktopCaptureConfig {
    fn from(v: DesktopCaptureConfigJson) -> Self {
        Self { fps: v.fps, jpeg_quality: v.jpeg_quality, display_index: v.display_index }
    }
}
```

Extend `JobRecord`:
```rust
pub struct JobRecord {
    // ...existing fields...
    #[serde(default)]
    pub desktop_capture_config: Option<DesktopCaptureConfigJson>,
}

impl JobRecord {
    pub fn is_desktop_capture(&self) -> bool {
        self.desktop_capture_config.is_some()
    }
}
```

Export from `crates/ahand-hub-store/src/lib.rs`:
```rust
pub use job_store::{DesktopCaptureConfigJson, JobRecord, /* ...existing... */};
```

**Important:** Task 5 will wire this field through DB persistence. For THIS task, it's only a struct field — existing code paths leave it as `None`. In-memory stores pass it through trivially; Postgres reads will produce `None` until Task 5 adds column handling. Task 4's tests use the in-memory store and therefore work end-to-end without Task 5.

Run: `cargo check --workspace`
Expected: all existing `NewJob` and `JobRecord` constructors still compile (the new field has `#[serde(default)]` and `Default for Option<T>` gives `None`, so any existing `..Default::default()` pattern is unchanged).

- [ ] **Step 1: Read the existing `handle_device_frame`** to find the match statement location and state struct shape.

Run: Read `crates/ahand-hub/src/http/jobs.rs` lines 150-280.
Expected: Locate `pub async fn handle_device_frame`, identify the `JobsState` (or similarly-named) struct it belongs to, and find where the struct is constructed (likely in `mod.rs` or `lib.rs`).

- [ ] **Step 2: Add the bus field to the state struct.**

Find the struct definition (likely `JobsState` in `jobs.rs`) and add:
```rust
pub desktop_bus: Arc<crate::desktop_bus::DesktopFrameBus>,
```

Find the constructor (`JobsState::new` or similar) and initialize:
```rust
desktop_bus: Arc::new(crate::desktop_bus::DesktopFrameBus::new()),
```

- [ ] **Step 3: Write the failing test first.** Add to the `#[cfg(test)] mod ...` block at the bottom of `jobs.rs`:

```rust
#[tokio::test]
async fn handle_device_frame_routes_desktop_frame_to_bus() {
    let state = test_support::fresh_jobs_state().await;  // (helper: see Step 6)

    // Create a desktop-capture job for device-1
    let job_id = state.create_desktop_job("device-1", 10, 70, 0).await
        .expect("create desktop job");

    // Subscribe BEFORE publishing
    let mut rx = state.desktop_bus.subscribe(&job_id);

    // Craft an envelope with a DesktopFrame
    let frame = ahand_protocol::DesktopFrame {
        job_id: job_id.clone(),
        frame_id: 0,
        width: 640,
        height: 480,
        mime: "image/jpeg".into(),
        data: vec![0xff, 0xd8, 0xff, 0xd9],  // minimal JPEG-ish
        captured_at_ms: 123,
    };
    let env = ahand_protocol::Envelope {
        device_id: "device-1".into(),
        seq: 1, ack: 0, ts_ms: 0,
        payload: Some(ahand_protocol::envelope::Payload::DesktopFrame(frame)),
        ..Default::default()
    };
    let bytes = prost::Message::encode_to_vec(&env);

    state.handle_device_frame("device-1", &bytes).await.unwrap();

    // Subscriber should have received the frame
    let got = rx.recv().await.expect("should receive frame");
    assert_eq!(got.frame_id, 0);
    assert_eq!(got.data.len(), 4);
}

#[tokio::test]
async fn handle_device_frame_desktop_unknown_job_errors() {
    let state = test_support::fresh_jobs_state().await;
    let frame = ahand_protocol::DesktopFrame {
        job_id: "ghost".into(), frame_id: 0, width: 1, height: 1,
        mime: "image/jpeg".into(), data: vec![], captured_at_ms: 0,
    };
    let env = ahand_protocol::Envelope {
        device_id: "device-1".into(),
        payload: Some(ahand_protocol::envelope::Payload::DesktopFrame(frame)),
        ..Default::default()
    };
    let bytes = prost::Message::encode_to_vec(&env);
    let err = state.handle_device_frame("device-1", &bytes).await.unwrap_err();
    assert!(err.to_string().contains("unknown"));
}

#[tokio::test]
async fn job_finished_closes_desktop_bus_session() {
    let state = test_support::fresh_jobs_state().await;
    let job_id = state.create_desktop_job("device-1", 10, 70, 0).await.unwrap();

    // Seed a frame so the bus entry exists
    let frame = ahand_protocol::DesktopFrame {
        job_id: job_id.clone(), frame_id: 0, width: 1, height: 1,
        mime: "image/jpeg".into(), data: vec![], captured_at_ms: 0,
    };
    state.desktop_bus.publish(&job_id, frame);

    // Send JobFinished
    let finished = ahand_protocol::JobFinished {
        job_id: job_id.clone(), exit_code: 0, error: String::new(),
    };
    let env = ahand_protocol::Envelope {
        device_id: "device-1".into(), seq: 2, ack: 0,
        payload: Some(ahand_protocol::envelope::Payload::JobFinished(finished)),
        ..Default::default()
    };
    let bytes = prost::Message::encode_to_vec(&env);
    state.handle_device_frame("device-1", &bytes).await.unwrap();

    // After close, a fresh publish on the same job_id behaves as a brand-new session
    // (no subscribers are connected, publish returns 0).
    let n = state.desktop_bus.publish(&job_id, ahand_protocol::DesktopFrame {
        job_id: job_id.clone(), frame_id: 99, width: 1, height: 1,
        mime: "image/jpeg".into(), data: vec![], captured_at_ms: 0,
    });
    assert_eq!(n, 0);
}
```

`test_support::fresh_jobs_state` and `create_desktop_job` are helpers you'll add in Step 6.

- [ ] **Step 4: Add the `DesktopFrame` match arm** to `handle_device_frame`, next to the existing `JobEvent` arm (mirror the stdout_chunk transition path):

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
    if let Err(err) = self
        .transition_job(&frame.job_id, JobStatus::Running, &format!("device:{device_id}"))
        .await
    {
        return self.handle_stale_device_frame_error(device_id, seq, ack, err);
    }
    let subscribers = self.desktop_bus.publish(&frame.job_id, frame);
    tracing::debug!(job_id = %job.id, subscribers, "desktop frame routed");
}
```

- [ ] **Step 5: Extend the `JobFinished` arm** to close the bus session:

Inside the existing `JobFinished` match arm, after the job state transition but before `observe_inbound`, add:
```rust
if job.is_desktop_capture() {
    self.desktop_bus.close(&finished.job_id);
}
```

`is_desktop_capture()` was added in Step 0 of this task. No further changes to the struct required here.

- [ ] **Step 6: Add test helpers** in the existing `#[cfg(test)] mod test_support` (or create it if absent) at the bottom of `jobs.rs`:

```rust
#[cfg(test)]
mod test_support {
    use super::*;
    pub async fn fresh_jobs_state() -> Arc<JobsState> { /* build an in-memory store, return */ }
}

#[cfg(test)]
impl JobsState {
    pub async fn create_desktop_job(
        &self, device_id: &str, fps: u32, quality: u32, display_index: u32,
    ) -> anyhow::Result<String> {
        // Mirror the production create-job path but with desktop_capture set.
        // Returns the generated job_id.
    }
}
```

Implement `fresh_jobs_state` by mirroring the setup used by existing tests in the same file (look for other `#[tokio::test]` fns — they'll show the in-memory store wiring).

- [ ] **Step 7: Run tests.**

Run: `cargo test -p ahand-hub http::jobs`
Expected: the three new tests pass; existing tests still pass.

- [ ] **Step 8: Commit.**

```bash
git add crates/ahand-hub-store/src/job_store.rs crates/ahand-hub-store/src/lib.rs crates/ahand-hub/src/http/jobs.rs
git commit -m "feat(hub): JobRecord.desktop_capture_config field + handle_device_frame routes DesktopFrame"
```

---

## Task 5: Hub Store — DB migration + Postgres persistence for `desktop_capture_config`

**Goal:** Add the DB migration for the new JSONB column and wire Postgres insert/read to persist the `desktop_capture_config` field added to `JobRecord` in Task 4. In-memory stores already pass the field through; only Postgres needs column handling.

**Files:**
- Create: `crates/ahand-hub-store/migrations/0002_add_desktop_capture_config.sql`
- Modify: `crates/ahand-hub-store/src/postgres.rs` — persist / hydrate the new column
- Modify: `crates/ahand-hub-store/tests/store_roundtrip.rs` — cover desktop-capture round-trip on both in-memory and Postgres backends

**Acceptance Criteria:**
- [ ] Migration adds nullable `desktop_capture_config JSONB` to `jobs`
- [ ] Existing tests still pass (column is additive / nullable)
- [ ] Round-trip test: insert job with desktop config → read back → config matches (memory and Postgres)
- [ ] Existing non-desktop jobs round-trip with `desktop_capture_config: None`

**Verify:** `cargo test -p ahand-hub-store` → all tests pass (including new round-trip).

**Steps:**

- [ ] **Step 1: Write the migration SQL.** Create `crates/ahand-hub-store/migrations/0002_add_desktop_capture_config.sql`:

```sql
ALTER TABLE jobs
    ADD COLUMN desktop_capture_config JSONB;
```

(Single additive nullable column — no backfill needed.)

- [ ] **Step 2: Confirm the struct field from Task 4.**

Verify `JobRecord.desktop_capture_config: Option<DesktopCaptureConfigJson>` exists (added in Task 4 Step 0). If missing, stop and complete Task 4 first.

- [ ] **Step 3: Update the Postgres insert / select** in `postgres.rs`:

Insert: change the `INSERT INTO jobs (...)` statement to include `desktop_capture_config` and bind `serde_json::to_value(&record.desktop_capture_config).unwrap_or(serde_json::Value::Null)`.

Select: change every `SELECT ... FROM jobs` statement to include `desktop_capture_config`. In the row-to-`JobRecord` mapping, deserialize with `row.try_get::<Option<serde_json::Value>, _>("desktop_capture_config")?` then `.and_then(|v| serde_json::from_value(v).ok())`.

If there are in-memory store implementations (`store::memory` or similar), add the field there too. Memory store changes are trivial — just pass the value through.

- [ ] **Step 4: Write the round-trip test.** In `crates/ahand-hub-store/tests/store_roundtrip.rs` (or create if absent), add:

```rust
#[tokio::test]
async fn desktop_capture_config_roundtrips() {
    let store = test_support::new_memory_store().await;
    let cfg = ahand_hub_store::DesktopCaptureConfigJson {
        fps: 10, jpeg_quality: 70, display_index: 0,
    };
    let id = store.create_job(ahand_hub_store::NewJob {
        device_id: "dev-1".into(),
        tool: "".into(),
        args: vec![],
        timeout_ms: 0,
        requested_by: "test".into(),
        desktop_capture_config: Some(cfg.clone()),
        // ... other fields with sensible defaults
    }).await.unwrap();

    let got = store.get_job(&id).await.unwrap().expect("job exists");
    assert!(got.is_desktop_capture());
    assert_eq!(got.desktop_capture_config, Some(cfg));
}

#[tokio::test]
async fn non_desktop_job_has_no_config() {
    let store = test_support::new_memory_store().await;
    let id = store.create_job(ahand_hub_store::NewJob {
        device_id: "dev-1".into(),
        tool: "ls".into(),
        args: vec!["-la".into()],
        timeout_ms: 60000,
        requested_by: "test".into(),
        desktop_capture_config: None,
        // ... other fields
    }).await.unwrap();

    let got = store.get_job(&id).await.unwrap().expect("job exists");
    assert!(!got.is_desktop_capture());
    assert!(got.desktop_capture_config.is_none());
}
```

Also add a Postgres-backed test gated on the same `#[cfg(feature = "pg")]` or `#[ignore]` pattern the existing Postgres tests use.

- [ ] **Step 5: Run migrations + tests.**

For the in-memory path:
```bash
cargo test -p ahand-hub-store
```

For Postgres (if the project uses a local DB for tests):
```bash
# check how existing migrations are applied — likely via sqlx migrate or a test harness
cargo test -p ahand-hub-store --features pg -- --ignored
```
(Adjust the command to match the project's existing pattern; read `crates/ahand-hub-store/tests/store_roundtrip.rs` to see how it drives migrations.)

- [ ] **Step 6: Verify the full workspace still compiles.**

Run: `cargo check --workspace`
Expected: clean. (Task 4 already ensured `NewJob` / `JobRecord` callers compile; this is just defensive.)

- [ ] **Step 7: Commit.**

```bash
git add crates/ahand-hub-store/migrations/0002_add_desktop_capture_config.sql crates/ahand-hub-store/src/postgres.rs crates/ahand-hub-store/tests/store_roundtrip.rs
git commit -m "feat(hub-store): persist desktop_capture_config JSONB column in Postgres + round-trip test"
```

---

## Task 6: Hub — `POST /api/jobs` desktop branch + audit + 409 concurrency

**Goal:** Accept `desktop_capture` in the job-create API, enforce one-session-per-device concurrency (409 on duplicate), write `desktop.capture.start` / `desktop.capture.rejected` audit events, and wire `desktop.capture.end` into the existing `JobFinished` cleanup path (from Task 4).

**Files:**
- Modify: `crates/ahand-hub/src/http/jobs.rs` — `create_job` handler, audit emission
- Modify: `crates/ahand-hub-store/src/audit_store.rs` — add event type constants
- Modify: `crates/ahand-hub/tests/job_flow.rs` (or create a new integration test file) — concurrency + audit coverage

**Acceptance Criteria:**
- [ ] `CreateJobRequest` DTO accepts `desktop_capture: Option<DesktopCaptureConfigJson>` (or nested JSON object matching the protobuf fields)
- [ ] Handler persists the config via `NewJob::desktop_capture_config`
- [ ] Handler returns 409 Conflict when an active desktop session already exists for the device
- [ ] `desktop.capture.start` audit event written on successful create
- [ ] `desktop.capture.rejected` audit event written on 409 or policy rejection
- [ ] `desktop.capture.end` audit event written on `JobFinished` (set `duration_ms` + `frame_count` + `total_bytes`)
- [ ] Integration test covers: successful create → audit, second create → 409 + audit, JobFinished → end audit

**Verify:** `cargo test -p ahand-hub --test job_flow desktop` → passes.

**Steps:**

- [ ] **Step 1: Add audit event type constants** in `crates/ahand-hub-store/src/audit_store.rs`:

```rust
pub const ACTION_DESKTOP_CAPTURE_START:    &str = "desktop.capture.start";
pub const ACTION_DESKTOP_CAPTURE_END:      &str = "desktop.capture.end";
pub const ACTION_DESKTOP_CAPTURE_REJECTED: &str = "desktop.capture.rejected";
```

Export them from `src/lib.rs` if the audit_store is re-exported there.

- [ ] **Step 2: Track per-session stats in `DesktopFrameBus`.**

For the `desktop.capture.end` audit we need `frame_count` and `total_bytes`. The bus is the natural place to track these — each `publish` bumps counters.

Extend `DesktopFrameBus` with per-session stats:

```rust
struct SessionEntry {
    tx: broadcast::Sender<Arc<DesktopFrame>>,
    stats: Arc<Mutex<SessionStats>>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SessionStats {
    pub frame_count: u64,
    pub total_bytes: u64,
    pub started_at_ms: u64,
}

pub struct DesktopFrameBus {
    sessions: DashMap<String, SessionEntry>,
}

impl DesktopFrameBus {
    pub fn publish(&self, job_id: &str, frame: DesktopFrame) -> usize {
        let size = frame.data.len() as u64;
        let entry = self.sessions.entry(job_id.to_string()).or_insert_with(|| SessionEntry {
            tx: broadcast::channel(CHANNEL_CAPACITY).0,
            stats: Arc::new(Mutex::new(SessionStats {
                started_at_ms: now_ms(),
                ..Default::default()
            })),
        });
        {
            let mut s = entry.stats.lock().unwrap();
            s.frame_count += 1;
            s.total_bytes += size;
        }
        let frame = Arc::new(frame);
        entry.tx.send(frame).map(|n| n).unwrap_or(0)
    }

    /// Take and clear the stats snapshot, returning it. Used by JobFinished
    /// cleanup to emit the desktop.capture.end audit.
    pub fn take_stats(&self, job_id: &str) -> Option<SessionStats> {
        self.sessions.get(job_id).map(|e| *e.stats.lock().unwrap())
    }
}
```

Use `std::sync::Mutex` for `Arc<Mutex<SessionStats>>` — the lock is held for a few nanoseconds so it's fine outside async. Add `now_ms()` as a small helper in the same file (or import from `crate::utils`).

Update the unit tests in `desktop_bus.rs` to verify stats tracking:

```rust
#[tokio::test]
async fn publish_tracks_stats() {
    let bus = DesktopFrameBus::new();
    bus.publish("job-1", make_frame(0));
    bus.publish("job-1", DesktopFrame { data: vec![0u8; 1000], ..make_frame(1) });
    let stats = bus.take_stats("job-1").unwrap();
    assert_eq!(stats.frame_count, 2);
    assert!(stats.total_bytes >= 1000);
    assert!(stats.started_at_ms > 0);
}
```

- [ ] **Step 3: Extend `CreateJobRequest` DTO** to accept desktop capture config.

In `crates/ahand-hub/src/http/jobs.rs`, find the existing `CreateJobRequest` struct and add:
```rust
pub struct CreateJobRequest {
    // ...existing...
    #[serde(default)]
    pub desktop_capture: Option<ahand_hub_store::DesktopCaptureConfigJson>,
}
```

- [ ] **Step 4: Modify the `create_job` handler** to branch on desktop capture:

```rust
pub async fn create_job(
    State(state): State<Arc<JobsState>>,
    // ...existing auth extractors...
    Json(req): Json<CreateJobRequest>,
) -> Result<Json<CreateJobResponse>, ApiError> {
    // ... existing device-online / auth checks ...

    // ── Desktop capture path ──────────────────────────────────
    if let Some(dc) = req.desktop_capture.clone() {
        // Concurrency check: one active desktop session per device
        let active_desktop = state.store
            .list_jobs_by_device(&req.device_id)
            .await?
            .into_iter()
            .filter(|j| j.is_desktop_capture() && !is_terminal_status(j.status))
            .count();
        if active_desktop >= 1 {
            state.audit.log(ahand_hub_store::AuditEntry {
                action: ahand_hub_store::ACTION_DESKTOP_CAPTURE_REJECTED.into(),
                resource_type: "device".into(),
                resource_id: req.device_id.clone(),
                actor: caller_id.clone(),
                detail: serde_json::json!({
                    "reason": "device already has an active desktop session",
                    "config": &dc,
                }),
                source_ip: client_ip.clone(),
                ..Default::default()
            }).await?;
            return Err(ApiError::conflict("device already has an active desktop session"));
        }

        // Create the job with desktop_capture_config populated
        let new_job = ahand_hub_store::NewJob {
            device_id: req.device_id.clone(),
            tool: String::new(),
            args: vec![],
            cwd: None,
            env: Default::default(),
            timeout_ms: 0,
            requested_by: caller_id.clone(),
            desktop_capture_config: Some(dc.clone()),
        };
        let job_id = state.store.create_job(new_job).await?;

        // Send JobRequest envelope to the device
        let env = ahand_protocol::Envelope {
            device_id: req.device_id.clone(),
            payload: Some(ahand_protocol::envelope::Payload::JobRequest(ahand_protocol::JobRequest {
                job_id: job_id.clone(),
                desktop_capture: Some(dc.clone().into()),
                ..Default::default()
            })),
            ..Default::default()
        };
        state.connections.send_to_device(&req.device_id, env).await?;

        // Audit: start
        state.audit.log(ahand_hub_store::AuditEntry {
            action: ahand_hub_store::ACTION_DESKTOP_CAPTURE_START.into(),
            resource_type: "job".into(),
            resource_id: job_id.clone(),
            actor: caller_id.clone(),
            detail: serde_json::json!({
                "device_id": req.device_id,
                "config": &dc,
            }),
            source_ip: client_ip.clone(),
            ..Default::default()
        }).await?;

        return Ok(Json(CreateJobResponse { job_id, status: JobStatus::Pending }));
    }

    // ── Existing exec / interactive paths below, unchanged ──
}
```

Adjust the code to match whatever the real struct shapes / trait methods are called in the project. `list_jobs_by_device` may have a different name — follow the pattern existing code uses.

- [ ] **Step 5: Wire `desktop.capture.end` into the `JobFinished` arm** in `handle_device_frame`. Inside the existing `JobFinished` match arm, after the state transition:

```rust
if job.is_desktop_capture() {
    let stats = self.desktop_bus.take_stats(&finished.job_id).unwrap_or_default();
    let now = chrono::Utc::now().timestamp_millis() as u64;
    let duration_ms = now.saturating_sub(stats.started_at_ms);
    let outcome = if finished.error.is_empty() { "completed" } else { "failed" };
    self.audit.log(ahand_hub_store::AuditEntry {
        action: ahand_hub_store::ACTION_DESKTOP_CAPTURE_END.into(),
        resource_type: "job".into(),
        resource_id: finished.job_id.clone(),
        actor: format!("device:{device_id}"),
        detail: serde_json::json!({
            "outcome": outcome,
            "duration_ms": duration_ms,
            "frame_count": stats.frame_count,
            "total_bytes": stats.total_bytes,
            "exit_code": finished.exit_code,
            "error": finished.error,
        }),
        ..Default::default()
    }).await?;
    self.desktop_bus.close(&finished.job_id);
}
```

(Note: this replaces the simpler `close` call from Task 4.)

- [ ] **Step 6: Write integration tests** in `crates/ahand-hub/tests/job_flow.rs` (or a new `desktop_flow.rs` file):

```rust
#[tokio::test]
async fn create_desktop_job_writes_start_audit() {
    let harness = test_support::TestHarness::new().await;
    harness.register_device("dev-1").await;

    let resp = harness.create_job_raw(serde_json::json!({
        "device_id": "dev-1",
        "desktop_capture": { "fps": 10, "jpeg_quality": 70, "display_index": 0 }
    })).await;
    assert_eq!(resp.status(), 200);

    let audits = harness.list_audits().await;
    assert!(audits.iter().any(|a| a.action == "desktop.capture.start"));
}

#[tokio::test]
async fn second_desktop_job_returns_409_and_rejected_audit() {
    let harness = test_support::TestHarness::new().await;
    harness.register_device("dev-1").await;
    harness.create_job_raw(serde_json::json!({
        "device_id": "dev-1", "desktop_capture": { "fps": 10, "jpeg_quality": 70, "display_index": 0 }
    })).await;

    let resp = harness.create_job_raw(serde_json::json!({
        "device_id": "dev-1", "desktop_capture": { "fps": 15, "jpeg_quality": 80, "display_index": 0 }
    })).await;
    assert_eq!(resp.status(), 409);

    let audits = harness.list_audits().await;
    assert!(audits.iter().any(|a| a.action == "desktop.capture.rejected"));
}

#[tokio::test]
async fn job_finished_writes_end_audit_with_stats() {
    let harness = test_support::TestHarness::new().await;
    harness.register_device("dev-1").await;
    let job_id = harness.create_desktop_job("dev-1", 10, 70).await;

    // Seed a couple of frames into the bus
    harness.state.desktop_bus.publish(&job_id, ahand_protocol::DesktopFrame {
        job_id: job_id.clone(), frame_id: 0, width: 1, height: 1,
        mime: "image/jpeg".into(), data: vec![0u8; 1234], captured_at_ms: 0,
    });
    harness.state.desktop_bus.publish(&job_id, ahand_protocol::DesktopFrame {
        job_id: job_id.clone(), frame_id: 1, width: 1, height: 1,
        mime: "image/jpeg".into(), data: vec![0u8; 2345], captured_at_ms: 0,
    });

    // Send JobFinished
    harness.send_device_envelope("dev-1", ahand_protocol::Envelope {
        device_id: "dev-1".into(),
        payload: Some(ahand_protocol::envelope::Payload::JobFinished(ahand_protocol::JobFinished {
            job_id: job_id.clone(), exit_code: 0, error: String::new(),
        })),
        ..Default::default()
    }).await;

    let audits = harness.list_audits().await;
    let end = audits.iter().find(|a| a.action == "desktop.capture.end").expect("end audit");
    assert_eq!(end.detail["frame_count"], 2);
    assert!(end.detail["total_bytes"].as_u64().unwrap() >= 1234 + 2345);
    assert_eq!(end.detail["outcome"], "completed");
}
```

`TestHarness`, `create_job_raw`, `list_audits`, `create_desktop_job`, `send_device_envelope` are helpers you either add or adapt from the existing `tests/support/` module. Follow the patterns already in `tests/job_flow.rs`.

- [ ] **Step 7: Run the full hub test suite.**

Run: `cargo test -p ahand-hub`
Expected: all new and existing tests pass.

- [ ] **Step 8: Commit.**

```bash
git add crates/ahand-hub-store/src/audit_store.rs crates/ahand-hub/src/http/jobs.rs crates/ahand-hub/src/desktop_bus.rs crates/ahand-hub/tests/
git commit -m "feat(hub): POST /api/jobs desktop branch + concurrency 409 + audit events"
```

---

## Task 7: Hub — `/api/desktop/token` + `/ws/desktop` endpoints

**Goal:** Issue one-time tokens (60 s TTL, single-use) for desktop sessions and serve the WebSocket gateway that streams paired text-metadata + binary-JPEG frames to the dashboard.

**Files:**
- Create: `crates/ahand-hub/src/http/desktop.rs`
- Modify: `crates/ahand-hub/src/http/mod.rs` — register routes and re-export the module
- Modify: `crates/ahand-hub/src/http/jobs.rs` (if the `JobsState` needs a `DesktopTokenStore` field)
- Modify: `crates/ahand-hub/tests/desktop_flow.rs` — add WS test

**Acceptance Criteria:**
- [ ] `POST /api/desktop/token` requires session JWT; returns `{token, expires_in}`; rejects if job doesn't exist, isn't a desktop job, or doesn't belong to the caller
- [ ] Token is single-use: second WS connect with the same token is rejected
- [ ] Token expires after 60 s
- [ ] `GET /ws/desktop?token=...&job_id=...` upgrades, subscribes to bus, streams paired frames
- [ ] Text frame format: `{"type":"frame", "frame_id": N, "width": W, "height": H, "captured_at_ms": T}`
- [ ] Followed immediately by binary WS frame containing raw JPEG bytes
- [ ] On `JobFinished` (bus closed), send `{"type":"ended", "exit_code": N}` then close
- [ ] On slow subscriber lag, skip frames silently (already handled by bus)
- [ ] Integration test: end-to-end subscribe → receive pairs → session ends

**Verify:** `cargo test -p ahand-hub desktop_flow` → passes.

**Steps:**

- [ ] **Step 1: Read `crates/ahand-hub/src/http/terminal.rs`** end-to-end to understand the one-time token pattern and WS upgrade structure — you will mirror it closely.

- [ ] **Step 2: Create `crates/ahand-hub/src/http/desktop.rs`** with the token store + handlers:

```rust
use crate::desktop_bus::DesktopFrameBus;
use crate::http::api_error::ApiError;
use axum::{
    Json,
    extract::{Query, State, WebSocketUpgrade, ws::{Message, WebSocket}},
    response::{IntoResponse, Response},
};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, info, warn};
use uuid::Uuid;

const TOKEN_TTL_SECS: u64 = 60;

#[derive(Debug, Clone)]
pub struct DesktopTokenEntry {
    pub job_id: String,
    pub caller: String,
    pub expires_at: Instant,
}

#[derive(Default)]
pub struct DesktopTokenStore {
    tokens: DashMap<String, DesktopTokenEntry>,
}

impl DesktopTokenStore {
    pub fn new() -> Self { Self::default() }
    pub fn issue(&self, job_id: String, caller: String) -> String {
        let token = Uuid::new_v4().to_string();
        self.tokens.insert(token.clone(), DesktopTokenEntry {
            job_id, caller, expires_at: Instant::now() + Duration::from_secs(TOKEN_TTL_SECS),
        });
        token
    }
    /// Consume the token, returning the entry if present and not expired.
    pub fn consume(&self, token: &str) -> Option<DesktopTokenEntry> {
        let (_, entry) = self.tokens.remove(token)?;
        if entry.expires_at < Instant::now() { return None; }
        Some(entry)
    }
}

#[derive(Deserialize)]
pub struct CreateTokenRequest { pub job_id: String }

#[derive(Serialize)]
pub struct CreateTokenResponse { pub token: String, pub expires_in: u64 }

pub async fn create_token(
    State(state): State<Arc<crate::AppState>>,
    // Reuse the same session extractor the terminal endpoint uses. If the
    // existing code uses a middleware-injected extractor, thread it here.
    caller: crate::http::auth::AuthenticatedUser,
    Json(req): Json<CreateTokenRequest>,
) -> Result<Json<CreateTokenResponse>, ApiError> {
    let job = state.jobs.store.get_job(&req.job_id).await?
        .ok_or_else(|| ApiError::not_found("job not found"))?;
    if !job.is_desktop_capture() {
        return Err(ApiError::bad_request("not a desktop capture job"));
    }
    if job.requested_by != caller.id {
        return Err(ApiError::forbidden("job does not belong to caller"));
    }
    let token = state.desktop_tokens.issue(job.id.clone(), caller.id.clone());
    Ok(Json(CreateTokenResponse { token, expires_in: TOKEN_TTL_SECS }))
}

#[derive(Deserialize)]
pub struct DesktopWsQuery { pub token: String, pub job_id: String }

pub async fn handle_desktop_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<crate::AppState>>,
    Query(q): Query<DesktopWsQuery>,
) -> Response {
    let Some(entry) = state.desktop_tokens.consume(&q.token) else {
        return (axum::http::StatusCode::UNAUTHORIZED, "invalid or expired token").into_response();
    };
    if entry.job_id != q.job_id {
        return (axum::http::StatusCode::UNAUTHORIZED, "token does not match job").into_response();
    }
    let bus = Arc::clone(&state.desktop_bus);
    let job_id = entry.job_id.clone();
    ws.on_upgrade(move |socket| desktop_ws_loop(socket, bus, job_id))
}

async fn desktop_ws_loop(mut socket: WebSocket, bus: Arc<DesktopFrameBus>, job_id: String) {
    let mut rx = bus.subscribe(&job_id);
    info!(job_id = %job_id, "desktop ws subscriber connected");

    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(frame) => {
                        let meta = serde_json::json!({
                            "type": "frame",
                            "frame_id": frame.frame_id,
                            "width": frame.width,
                            "height": frame.height,
                            "captured_at_ms": frame.captured_at_ms,
                        }).to_string();
                        if socket.send(Message::Text(meta.into())).await.is_err() { break; }
                        if socket.send(Message::Binary(frame.data.clone().into())).await.is_err() { break; }
                    }
                    Err(RecvError::Lagged(_)) => {
                        // Silently skip — dashboard will see fps dip, that's the
                        // expected "drop on slow" behavior.
                        continue;
                    }
                    Err(RecvError::Closed) => {
                        let ended = serde_json::json!({"type": "ended", "exit_code": 0}).to_string();
                        let _ = socket.send(Message::Text(ended.into())).await;
                        break;
                    }
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(p))) => { let _ = socket.send(Message::Pong(p)).await; }
                    _ => { /* Phase 1 ignores inbound text/binary from dashboard */ }
                }
            }
        }
    }
    debug!(job_id = %job_id, "desktop ws subscriber disconnected");
}
```

(Exact types for the AppState / AuthenticatedUser / ApiError extractors will match what the existing codebase uses. Trace the `terminal.rs` imports to align.)

- [ ] **Step 3: Add `desktop_tokens: Arc<DesktopTokenStore>` to the app state** wherever `JobsState` or the top-level state is constructed, and expose it to the routes. Initialize with `Arc::new(DesktopTokenStore::new())` at startup.

- [ ] **Step 4: Register the routes** in `crates/ahand-hub/src/http/mod.rs`:

```rust
pub mod desktop;

// In the router builder (`build_router` or similar):
.route("/api/desktop/token", post(desktop::create_token))
.route("/ws/desktop", get(desktop::handle_desktop_ws))
```

- [ ] **Step 5: Write the integration test.** Add to `crates/ahand-hub/tests/desktop_flow.rs` (or extend `tests/job_flow.rs`):

```rust
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as WsMessage;

#[tokio::test]
async fn ws_desktop_streams_paired_frames() {
    let harness = test_support::TestHarness::new().await;
    harness.register_device("dev-1").await;
    let job_id = harness.create_desktop_job_as_user("dev-1", "alice").await;
    let token = harness.issue_desktop_token(&job_id, "alice").await;

    // Seed a frame into the bus BEFORE subscriber connects — they should
    // miss this frame (publish-without-subscribers), then catch the next one.
    harness.state.desktop_bus.publish(&job_id, test_support::make_jpeg_frame(&job_id, 0, b"frameA"));

    let url = format!("ws://{}/ws/desktop?token={}&job_id={}", harness.addr(), token, job_id);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.expect("ws connect");

    // Publish after subscribe
    harness.state.desktop_bus.publish(&job_id, test_support::make_jpeg_frame(&job_id, 1, b"frameB"));

    // Expect a text metadata frame, then a binary frame
    let m1 = ws.next().await.unwrap().unwrap();
    let m2 = ws.next().await.unwrap().unwrap();
    let meta: serde_json::Value = match m1 {
        WsMessage::Text(t) => serde_json::from_str(&t).unwrap(),
        other => panic!("expected text metadata, got {:?}", other),
    };
    assert_eq!(meta["type"], "frame");
    assert_eq!(meta["frame_id"], 1);
    match m2 {
        WsMessage::Binary(bytes) => assert_eq!(bytes.as_ref(), b"frameB"),
        other => panic!("expected binary, got {:?}", other),
    }
}

#[tokio::test]
async fn ws_desktop_ends_on_job_finished() {
    let harness = test_support::TestHarness::new().await;
    harness.register_device("dev-1").await;
    let job_id = harness.create_desktop_job_as_user("dev-1", "alice").await;
    let token = harness.issue_desktop_token(&job_id, "alice").await;
    let url = format!("ws://{}/ws/desktop?token={}&job_id={}", harness.addr(), token, job_id);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // Simulate the device finishing the job
    harness.state.desktop_bus.close(&job_id);

    // Should receive {"type":"ended"} and then the socket closes
    let m = ws.next().await.unwrap().unwrap();
    match m {
        WsMessage::Text(t) => {
            let v: serde_json::Value = serde_json::from_str(&t).unwrap();
            assert_eq!(v["type"], "ended");
        }
        other => panic!("expected ended text, got {:?}", other),
    }
}

#[tokio::test]
async fn token_is_single_use() {
    let harness = test_support::TestHarness::new().await;
    harness.register_device("dev-1").await;
    let job_id = harness.create_desktop_job_as_user("dev-1", "alice").await;
    let token = harness.issue_desktop_token(&job_id, "alice").await;
    let url = format!("ws://{}/ws/desktop?token={}&job_id={}", harness.addr(), token, job_id);

    // First connection succeeds
    let (_ws1, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // Second connection with same token fails
    let res = tokio_tungstenite::connect_async(&url).await;
    assert!(res.is_err(), "second use of token should fail");
}
```

`issue_desktop_token` is a test helper that calls `create_token` through the authenticated user path; implement it to mirror how `tests/job_flow.rs` already drives authenticated requests.

- [ ] **Step 6: Run tests.**

Run: `cargo test -p ahand-hub desktop_flow`
Expected: all three tests pass.

- [ ] **Step 7: Commit.**

```bash
git add crates/ahand-hub/src/http/desktop.rs crates/ahand-hub/src/http/mod.rs crates/ahand-hub/src/lib.rs crates/ahand-hub/tests/desktop_flow.rs
git commit -m "feat(hub): /api/desktop/token + /ws/desktop endpoints with paired frame format"
```

---

## Task 8: Dashboard — `use-desktop-stream` hook

**Goal:** Implement the React hook that manages the desktop session lifecycle (job create → token → WS → frames → end), with EMA stats for the debug panel. TDD with vitest — fake `WebSocket` and `fetch` in unit tests.

**Files:**
- Create: `apps/hub-dashboard/src/hooks/use-desktop-stream.ts`
- Create: `apps/hub-dashboard/tests/desktop-stream.test.ts`

**Acceptance Criteria:**
- [ ] Hook exports `useDesktopStream(deviceId)` returning `{state, stats, start, stop, onFrame}`
- [ ] State machine: `idle → requesting-token → connecting → streaming → ended | error`
- [ ] `start()` calls POST `/api/proxy/api/jobs` → POST `/api/proxy/api/desktop/token` → opens WS
- [ ] Text-frame parsing: `{type:"frame"}` stores metadata, `{type:"ended"}` transitions state
- [ ] Binary frame without preceding metadata logs a warn (console) but doesn't crash
- [ ] `bitmap.close()` called after consumer callbacks run
- [ ] `stop()` posts cancel, closes WS, returns to `idle`
- [ ] Unit tests cover each transition + error paths + frame parsing

**Verify:** `pnpm --filter hub-dashboard test desktop-stream` → passes.

**Steps:**

- [ ] **Step 1: Look at the existing job-output hook** for conventions.

Run: Read `apps/hub-dashboard/src/hooks/use-job-output.ts`.
Expected: see how the project structures React hooks, where `fetch` proxy URLs come from, and how WebSocket URL resolution is done (there should be a utility for converting the HTTP proxy base to a WS URL).

- [ ] **Step 2: Write the failing tests.** Create `apps/hub-dashboard/tests/desktop-stream.test.ts`:

```typescript
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";
import { useDesktopStream } from "../src/hooks/use-desktop-stream";

// Mock global fetch
const mockFetch = vi.fn();
globalThis.fetch = mockFetch as unknown as typeof fetch;

// Mock WebSocket
class FakeWS {
  static instances: FakeWS[] = [];
  url: string;
  binaryType = "arraybuffer";
  readyState = 0;
  onopen: ((e: Event) => void) | null = null;
  onmessage: ((e: MessageEvent) => void) | null = null;
  onerror: ((e: Event) => void) | null = null;
  onclose: ((e: CloseEvent) => void) | null = null;
  sent: string[] = [];
  constructor(url: string) {
    this.url = url;
    FakeWS.instances.push(this);
    setTimeout(() => { this.readyState = 1; this.onopen?.(new Event("open")); }, 0);
  }
  send(data: string) { this.sent.push(data); }
  close() { this.readyState = 3; this.onclose?.(new CloseEvent("close")); }
  emit(data: unknown) { this.onmessage?.(new MessageEvent("message", { data })); }
}
// @ts-expect-error override
globalThis.WebSocket = FakeWS;

// Mock createImageBitmap to return a fake bitmap with close()
const mockCreateImageBitmap = vi.fn(async (_blob: Blob) => ({
  close: vi.fn(),
  width: 640,
  height: 480,
} as unknown as ImageBitmap));
// @ts-expect-error override
globalThis.createImageBitmap = mockCreateImageBitmap;

beforeEach(() => {
  FakeWS.instances = [];
  mockFetch.mockReset();
  mockCreateImageBitmap.mockClear();
});

describe("useDesktopStream", () => {
  it("starts idle", () => {
    const { result } = renderHook(() => useDesktopStream("dev-1"));
    expect(result.current.state.kind).toBe("idle");
  });

  it("transitions idle → requesting-token → connecting → streaming", async () => {
    mockFetch
      .mockResolvedValueOnce({ ok: true, json: async () => ({ job_id: "job-1", status: "pending" }) })
      .mockResolvedValueOnce({ ok: true, json: async () => ({ token: "tok-1", expires_in: 60 }) });

    const { result } = renderHook(() => useDesktopStream("dev-1"));
    act(() => { result.current.start(); });
    await waitFor(() => expect(result.current.state.kind).toBe("connecting"));
    // First ws instance opens; then we emit a frame
    await waitFor(() => expect(FakeWS.instances.length).toBeGreaterThan(0));
    const ws = FakeWS.instances[0];
    // Text metadata, then binary
    ws.emit(JSON.stringify({ type: "frame", frame_id: 0, width: 640, height: 480, captured_at_ms: Date.now() - 20 }));
    const buf = new ArrayBuffer(100);
    ws.emit(buf);
    await waitFor(() => expect(result.current.state.kind).toBe("streaming"));
    await waitFor(() => expect(result.current.stats.frameCount).toBe(1));
  });

  it("fires onFrame callback with bitmap and meta", async () => {
    mockFetch
      .mockResolvedValueOnce({ ok: true, json: async () => ({ job_id: "job-1" }) })
      .mockResolvedValueOnce({ ok: true, json: async () => ({ token: "tok-1", expires_in: 60 }) });
    const { result } = renderHook(() => useDesktopStream("dev-1"));
    const cb = vi.fn();
    act(() => { result.current.onFrame(cb); });
    act(() => { result.current.start(); });
    await waitFor(() => expect(FakeWS.instances.length).toBeGreaterThan(0));
    const ws = FakeWS.instances[0];
    ws.emit(JSON.stringify({ type: "frame", frame_id: 42, width: 10, height: 20, captured_at_ms: 1000 }));
    ws.emit(new ArrayBuffer(4));
    await waitFor(() => expect(cb).toHaveBeenCalledTimes(1));
    expect(cb.mock.calls[0][1].frameId).toBe(42);
  });

  it("transitions to ended on {type:ended}", async () => {
    mockFetch
      .mockResolvedValueOnce({ ok: true, json: async () => ({ job_id: "job-1" }) })
      .mockResolvedValueOnce({ ok: true, json: async () => ({ token: "tok-1", expires_in: 60 }) });
    const { result } = renderHook(() => useDesktopStream("dev-1"));
    act(() => { result.current.start(); });
    await waitFor(() => expect(FakeWS.instances.length).toBeGreaterThan(0));
    const ws = FakeWS.instances[0];
    ws.emit(JSON.stringify({ type: "ended", exit_code: 0 }));
    await waitFor(() => expect(result.current.state.kind).toBe("ended"));
  });

  it("transitions to error when token fetch fails", async () => {
    mockFetch
      .mockResolvedValueOnce({ ok: true, json: async () => ({ job_id: "job-1" }) })
      .mockResolvedValueOnce({ ok: false, status: 401, text: async () => "bad token" });
    const { result } = renderHook(() => useDesktopStream("dev-1"));
    act(() => { result.current.start(); });
    await waitFor(() => expect(result.current.state.kind).toBe("error"));
  });

  it("binary frame without preceding meta warns but does not crash", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    mockFetch
      .mockResolvedValueOnce({ ok: true, json: async () => ({ job_id: "job-1" }) })
      .mockResolvedValueOnce({ ok: true, json: async () => ({ token: "tok-1", expires_in: 60 }) });
    const { result } = renderHook(() => useDesktopStream("dev-1"));
    act(() => { result.current.start(); });
    await waitFor(() => expect(FakeWS.instances.length).toBeGreaterThan(0));
    const ws = FakeWS.instances[0];
    // Binary without preceding text
    ws.emit(new ArrayBuffer(100));
    // State should NOT be error (non-fatal)
    expect(result.current.state.kind).not.toBe("error");
    expect(warn).toHaveBeenCalled();
    warn.mockRestore();
  });
});
```

- [ ] **Step 3: Run tests, confirm they fail to compile** (the hook file doesn't exist yet).

Run: `pnpm --filter hub-dashboard test desktop-stream`
Expected: import error because `use-desktop-stream.ts` doesn't exist.

- [ ] **Step 4: Implement the hook.** Create `apps/hub-dashboard/src/hooks/use-desktop-stream.ts`:

```typescript
"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

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
  | { kind: "ended"; exitCode: number }
  | { kind: "error"; message: string };

export type DesktopStats = {
  fpsEma: number;
  lastFrameBytes: number;
  totalBytes: number;
  frameCount: number;
  droppedCount: number;
  latencyMsEma: number;
};

const INITIAL_STATS: DesktopStats = {
  fpsEma: 0,
  lastFrameBytes: 0,
  totalBytes: 0,
  frameCount: 0,
  droppedCount: 0,
  latencyMsEma: 0,
};

const EMA_ALPHA = 0.2;

type FrameCallback = (bitmap: ImageBitmap, meta: DesktopFrameMeta) => void;

function hubWsUrl(path: string): string {
  // Resolve the WS base URL same way the terminal hook does. If the
  // environment variable is absent, fall back to location.host.
  const base = process.env.NEXT_PUBLIC_HUB_WS_BASE ?? `${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}`;
  return `${base}${path}`;
}

export function useDesktopStream(deviceId: string) {
  const [state, setState] = useState<DesktopStreamState>({ kind: "idle" });
  const [stats, setStats] = useState<DesktopStats>(INITIAL_STATS);
  const wsRef = useRef<WebSocket | null>(null);
  const pendingMetaRef = useRef<{
    frameId: number; width: number; height: number; capturedAtMs: number;
  } | null>(null);
  const jobIdRef = useRef<string | null>(null);
  const lastArrivalRef = useRef<number | null>(null);
  const frameCallbacksRef = useRef<Set<FrameCallback>>(new Set());

  const cleanup = useCallback(() => {
    wsRef.current?.close();
    wsRef.current = null;
    pendingMetaRef.current = null;
    lastArrivalRef.current = null;
  }, []);

  const start = useCallback(async (opts?: { fps?: number; jpegQuality?: number }) => {
    setState({ kind: "requesting-token" });
    setStats(INITIAL_STATS);
    try {
      // 1. Create job
      const jobRes = await fetch("/api/proxy/api/jobs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          device_id: deviceId,
          desktop_capture: {
            fps: opts?.fps ?? 10,
            jpeg_quality: opts?.jpegQuality ?? 70,
            display_index: 0,
          },
        }),
      });
      if (!jobRes.ok) throw new Error(`create job failed: ${jobRes.status}`);
      const job = await jobRes.json();
      jobIdRef.current = job.job_id;

      // 2. Request token
      const tokRes = await fetch("/api/proxy/api/desktop/token", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ job_id: job.job_id }),
      });
      if (!tokRes.ok) throw new Error(`token failed: ${tokRes.status}`);
      const tok = await tokRes.json();

      // 3. Open WS
      setState({ kind: "connecting" });
      const ws = new WebSocket(hubWsUrl(`/ws/desktop?token=${encodeURIComponent(tok.token)}&job_id=${encodeURIComponent(job.job_id)}`));
      ws.binaryType = "arraybuffer";
      wsRef.current = ws;

      ws.onmessage = async (ev) => {
        if (typeof ev.data === "string") {
          try {
            const parsed = JSON.parse(ev.data);
            if (parsed.type === "frame") {
              pendingMetaRef.current = {
                frameId: parsed.frame_id,
                width: parsed.width,
                height: parsed.height,
                capturedAtMs: parsed.captured_at_ms,
              };
            } else if (parsed.type === "ended") {
              setState({ kind: "ended", exitCode: parsed.exit_code ?? 0 });
              cleanup();
            }
          } catch (err) {
            console.warn("desktop ws: bad text frame", err);
          }
        } else {
          const meta = pendingMetaRef.current;
          if (!meta) {
            console.warn("desktop ws: binary frame without preceding metadata");
            return;
          }
          pendingMetaRef.current = null;
          const buf = ev.data as ArrayBuffer;
          const byteLength = buf.byteLength;
          const receivedAtMs = Date.now();
          let bitmap: ImageBitmap;
          try {
            bitmap = await createImageBitmap(new Blob([buf], { type: "image/jpeg" }));
          } catch (err) {
            console.warn("desktop ws: createImageBitmap failed", err);
            return;
          }
          const fullMeta: DesktopFrameMeta = { ...meta, receivedAtMs, byteLength };
          setState({ kind: "streaming", lastFrame: fullMeta });
          setStats((prev) => {
            const dt = lastArrivalRef.current == null ? null : receivedAtMs - lastArrivalRef.current;
            lastArrivalRef.current = receivedAtMs;
            const instFps = dt && dt > 0 ? 1000 / dt : prev.fpsEma;
            const latency = Math.max(0, receivedAtMs - meta.capturedAtMs);
            return {
              frameCount: prev.frameCount + 1,
              droppedCount: prev.droppedCount,
              lastFrameBytes: byteLength,
              totalBytes: prev.totalBytes + byteLength,
              fpsEma: prev.fpsEma === 0 ? instFps : (1 - EMA_ALPHA) * prev.fpsEma + EMA_ALPHA * instFps,
              latencyMsEma: prev.latencyMsEma === 0 ? latency : (1 - EMA_ALPHA) * prev.latencyMsEma + EMA_ALPHA * latency,
            };
          });
          // Hand off to consumers; consumer is responsible for calling
          // bitmap.close() once it's done drawing.
          for (const cb of frameCallbacksRef.current) {
            cb(bitmap, fullMeta);
          }
        }
      };
      ws.onclose = () => {
        // Only transition to error if we're still in connecting/streaming;
        // an expected `ended` will have already cleaned up.
        setState((prev) => prev.kind === "ended" ? prev : { kind: "error", message: "websocket closed" });
      };
      ws.onerror = () => {
        setState({ kind: "error", message: "websocket error" });
      };
    } catch (err) {
      setState({ kind: "error", message: (err as Error).message });
      cleanup();
    }
  }, [deviceId, cleanup]);

  const stop = useCallback(async () => {
    const jobId = jobIdRef.current;
    if (jobId) {
      // Best-effort cancel — we don't await the response status
      fetch(`/api/proxy/api/jobs/${jobId}/cancel`, { method: "POST" }).catch(() => {});
    }
    cleanup();
    setState({ kind: "idle" });
  }, [cleanup]);

  const onFrame = useCallback((cb: FrameCallback) => {
    frameCallbacksRef.current.add(cb);
    return () => { frameCallbacksRef.current.delete(cb); };
  }, []);

  useEffect(() => cleanup, [cleanup]);

  return useMemo(() => ({ state, stats, start, stop, onFrame }), [state, stats, start, stop, onFrame]);
}
```

**Important:** the consumer of `onFrame` (the canvas component in Task 9) is responsible for calling `bitmap.close()` after drawing, NOT this hook. The hook hands off ownership.

- [ ] **Step 5: Run tests.**

Run: `pnpm --filter hub-dashboard test desktop-stream`
Expected: all 6 tests pass.

- [ ] **Step 6: Commit.**

```bash
git add apps/hub-dashboard/src/hooks/use-desktop-stream.ts apps/hub-dashboard/tests/desktop-stream.test.ts
git commit -m "feat(dashboard): useDesktopStream hook with state machine and EMA stats"
```

---

## Task 9: Dashboard — canvas renderer + debug panel components

**Goal:** Build the two leaf components: `DeviceDesktopCanvas` (pure renderer with correct `bitmap.close()` lifecycle) and `DeviceDesktopDebug` (compact one-row stats display with expandable details).

**Files:**
- Create: `apps/hub-dashboard/src/components/device-desktop-canvas.tsx`
- Create: `apps/hub-dashboard/src/components/device-desktop-debug.tsx`
- Modify: `apps/hub-dashboard/src/app/globals.css` (or the relevant stylesheet) — small set of classes for the desktop panel

**Acceptance Criteria:**
- [ ] `DeviceDesktopCanvas` accepts a `ref` for the canvas element
- [ ] `DeviceDesktopCanvas` displays a placeholder message when state is not `streaming`
- [ ] `DeviceDesktopDebug` renders a one-row status strip: badge, frame#, fps, last size, total bytes, latency, resolution
- [ ] Badge colors: gray (idle), yellow (connecting), green (streaming), dark (ended), red (error)
- [ ] Clicking the badge toggles an expanded detail panel showing the last 5 errors, job id, and WS URL
- [ ] Both components are fully-typed and accept their props without any `any`
- [ ] Import in a throwaway test file to verify they compile

**Verify:** `pnpm --filter hub-dashboard build` → succeeds (type check catches issues).

**Steps:**

- [ ] **Step 1: Create the canvas component.** `apps/hub-dashboard/src/components/device-desktop-canvas.tsx`:

```tsx
"use client";

import { forwardRef, type Ref } from "react";
import type { DesktopStreamState } from "../hooks/use-desktop-stream";

type Props = {
  state: DesktopStreamState;
};

export const DeviceDesktopCanvas = forwardRef(function DeviceDesktopCanvas(
  { state }: Props,
  ref: Ref<HTMLCanvasElement>,
) {
  const showPlaceholder = state.kind !== "streaming";
  return (
    <div className="device-desktop-canvas-wrap">
      <canvas ref={ref} className="device-desktop-canvas" />
      {showPlaceholder && (
        <div className="device-desktop-canvas-placeholder">
          {state.kind === "idle" && "Click Start to begin remote desktop session."}
          {state.kind === "requesting-token" && "Creating session..."}
          {state.kind === "connecting" && "Connecting..."}
          {state.kind === "ended" && `Session ended (exit code ${state.exitCode})`}
          {state.kind === "error" && `Error: ${state.message}`}
        </div>
      )}
    </div>
  );
});
```

- [ ] **Step 2: Create the debug panel.** `apps/hub-dashboard/src/components/device-desktop-debug.tsx`:

```tsx
"use client";

import { useState } from "react";
import type { DesktopStats, DesktopStreamState } from "../hooks/use-desktop-stream";

type Props = {
  state: DesktopStreamState;
  stats: DesktopStats;
  details?: { jobId: string | null; wsUrl: string | null; recentErrors: string[] };
};

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

function badgeFor(state: DesktopStreamState): { color: string; label: string } {
  switch (state.kind) {
    case "idle": return { color: "gray", label: "idle" };
    case "requesting-token":
    case "connecting": return { color: "yellow", label: state.kind };
    case "streaming": return { color: "green", label: "streaming" };
    case "ended": return { color: "dark", label: "ended" };
    case "error": return { color: "red", label: "error" };
  }
}

export function DeviceDesktopDebug({ state, stats, details }: Props) {
  const [expanded, setExpanded] = useState(false);
  const badge = badgeFor(state);
  const lastRes = state.kind === "streaming" && state.lastFrame
    ? `${state.lastFrame.width}×${state.lastFrame.height}`
    : "—";

  return (
    <div className="device-desktop-debug">
      <div className="device-desktop-debug-row">
        <button
          type="button"
          className={`device-desktop-debug-badge device-desktop-debug-badge-${badge.color}`}
          onClick={() => setExpanded((v) => !v)}
          aria-expanded={expanded}
        >
          {badge.label}
        </button>
        <span>frame #{stats.frameCount}</span>
        <span>·</span>
        <span>fps: {stats.fpsEma.toFixed(1)}</span>
        <span>·</span>
        <span>last: {formatBytes(stats.lastFrameBytes)}</span>
        <span>·</span>
        <span>total: {formatBytes(stats.totalBytes)}</span>
        <span>·</span>
        <span>latency: ~{stats.latencyMsEma.toFixed(0)} ms</span>
        <span>·</span>
        <span>res: {lastRes}</span>
      </div>
      {expanded && (
        <div className="device-desktop-debug-details">
          <dl>
            <dt>Job ID</dt><dd>{details?.jobId ?? "—"}</dd>
            <dt>WS URL</dt><dd>{details?.wsUrl ?? "—"}</dd>
            <dt>Recent errors</dt>
            <dd>
              {details?.recentErrors?.length
                ? <ul>{details.recentErrors.map((e, i) => <li key={i}>{e}</li>)}</ul>
                : "none"}
            </dd>
          </dl>
        </div>
      )}
    </div>
  );
}
```

- [ ] **Step 3: Add the styles.** Append to `apps/hub-dashboard/src/app/globals.css` (or the nearest existing stylesheet — match where `device-tabs-panel` is defined):

```css
.device-desktop-canvas-wrap { position: relative; width: 100%; min-height: 320px; background: #111; display: flex; align-items: center; justify-content: center; }
.device-desktop-canvas { max-width: 100%; max-height: 70vh; image-rendering: auto; object-fit: contain; display: block; }
.device-desktop-canvas-placeholder { position: absolute; color: #888; font-family: var(--font-mono, monospace); text-align: center; padding: 2rem; }
.device-desktop-debug { font-family: var(--font-mono, monospace); font-size: 0.85rem; padding: 0.5rem 0.75rem; background: #1a1a1a; color: #bbb; border-top: 1px solid #333; }
.device-desktop-debug-row { display: flex; gap: 0.5rem; align-items: center; flex-wrap: wrap; }
.device-desktop-debug-badge { border: none; border-radius: 3px; padding: 0.1rem 0.5rem; color: white; cursor: pointer; font-family: inherit; font-size: inherit; }
.device-desktop-debug-badge-gray { background: #555; }
.device-desktop-debug-badge-yellow { background: #b57a00; }
.device-desktop-debug-badge-green { background: #2e7d32; }
.device-desktop-debug-badge-dark { background: #222; }
.device-desktop-debug-badge-red { background: #b00020; }
.device-desktop-debug-details { margin-top: 0.5rem; padding-top: 0.5rem; border-top: 1px dashed #333; }
.device-desktop-debug-details dl { display: grid; grid-template-columns: auto 1fr; gap: 0.25rem 0.75rem; }
.device-desktop-debug-details dt { color: #888; }
```

- [ ] **Step 4: Verify compilation.**

Run: `pnpm --filter hub-dashboard build`
Expected: build succeeds (the new components have no call sites yet, but the TypeScript type check exercises their prop types).

- [ ] **Step 5: Commit.**

```bash
git add apps/hub-dashboard/src/components/device-desktop-canvas.tsx apps/hub-dashboard/src/components/device-desktop-debug.tsx apps/hub-dashboard/src/app/globals.css
git commit -m "feat(dashboard): canvas and debug panel components for desktop session"
```

---

## Task 10: Dashboard — `DeviceDesktop` + tabs integration + E2E smoke

**Goal:** Wire the hook and the two components into a top-level `DeviceDesktop` tab component; add the tab to `device-tabs.tsx`; verify end-to-end against a running daemon+hub+dashboard stack on macOS.

**Files:**
- Create: `apps/hub-dashboard/src/components/device-desktop.tsx`
- Modify: `apps/hub-dashboard/src/components/device-tabs.tsx`

**Acceptance Criteria:**
- [ ] `DeviceDesktop` owns: start/stop buttons, `DeviceDesktopCanvas`, `DeviceDesktopDebug`
- [ ] `useEffect` cleanup calls `stop()` on unmount so page nav/tab switch ends the session
- [ ] Canvas `drawImage` happens inside the `onFrame` callback and calls `bitmap.close()` after drawing
- [ ] Canvas internal width/height match the streamed resolution
- [ ] `device-tabs.tsx` gains a `"desktop"` tab value that's only visible when `online`
- [ ] Manual E2E smoke test (Steps 5–7) passes

**Verify:** `pnpm --filter hub-dashboard build && pnpm --filter hub-dashboard test` → clean.

**Steps:**

- [ ] **Step 1: Create `DeviceDesktop`.** `apps/hub-dashboard/src/components/device-desktop.tsx`:

```tsx
"use client";

import { useEffect, useRef } from "react";
import { useDesktopStream } from "../hooks/use-desktop-stream";
import { DeviceDesktopCanvas } from "./device-desktop-canvas";
import { DeviceDesktopDebug } from "./device-desktop-debug";

export function DeviceDesktop({ deviceId }: { deviceId: string }) {
  const stream = useDesktopStream(deviceId);
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    return stream.onFrame((bitmap, meta) => {
      const canvas = canvasRef.current;
      const ctx = canvas?.getContext("2d");
      if (!canvas || !ctx) { bitmap.close(); return; }
      if (canvas.width !== meta.width || canvas.height !== meta.height) {
        canvas.width = meta.width;
        canvas.height = meta.height;
      }
      ctx.drawImage(bitmap, 0, 0);
      bitmap.close();
    });
  }, [stream]);

  // Stop session on unmount (page nav, tab change, etc.)
  useEffect(() => {
    return () => { stream.stop(); };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const isRunning = stream.state.kind === "streaming"
    || stream.state.kind === "requesting-token"
    || stream.state.kind === "connecting";

  return (
    <div className="device-desktop-panel">
      <div className="device-desktop-controls">
        {!isRunning && (
          <button
            type="button"
            className="device-desktop-button"
            onClick={() => { stream.start(); }}
            disabled={stream.state.kind === "requesting-token" || stream.state.kind === "connecting"}
          >
            Start
          </button>
        )}
        {isRunning && (
          <button
            type="button"
            className="device-desktop-button"
            onClick={() => { stream.stop(); }}
          >
            Stop
          </button>
        )}
      </div>
      <DeviceDesktopCanvas ref={canvasRef} state={stream.state} />
      <DeviceDesktopDebug state={stream.state} stats={stream.stats} />
    </div>
  );
}
```

- [ ] **Step 2: Update `device-tabs.tsx`.** Modify the existing file:

```tsx
// At the top, import the new component:
import { DeviceDesktop } from "./device-desktop";

// Change the tab state type:
const [tab, setTab] = useState<"jobs" | "terminal" | "desktop">(online ? "terminal" : "jobs");

// Inside the header JSX, after the Terminal tab button:
{online && (
  <button
    className={`device-tab ${tab === "desktop" ? "device-tab-active" : ""}`}
    onClick={() => setTab("desktop")}
  >
    Desktop
  </button>
)}

// After the Terminal content block:
{tab === "desktop" && online && (
  <DeviceDesktop deviceId={deviceId} />
)}
```

- [ ] **Step 3: Type-check and test the dashboard.**

Run: `pnpm --filter hub-dashboard build && pnpm --filter hub-dashboard test`
Expected: clean.

- [ ] **Step 4: Local stack E2E smoke.** Start the full stack on macOS. (Check [README.md](../../../README.md) or existing docs for exact commands; the pattern is typically:)

```bash
# Terminal 1: start the hub
cargo run -p ahand-hub

# Terminal 2: start the daemon with desktop capture policy enabled
# (edit your ~/.ahand/ahandd.toml first to set [policy] allow_desktop_capture = true)
cargo run -p ahandd

# Terminal 3: start the dashboard
pnpm --filter hub-dashboard dev
```

Open http://localhost:3000, log in, navigate to your device, click the Desktop tab.

- [ ] **Step 5: Manual E2E verification — golden path.**

- Click **Start**.
- Within 1–2 seconds, the canvas should show the live contents of the primary display (your current macOS screen).
- The debug row should read something like `[🟢 streaming] frame #N · fps: 9.x · last: ... · total: ... · latency: ~xx ms · res: 2560×1600`.
- Click **Stop**. The canvas placeholder should say "idle".
- Click **Start** again. It should resume cleanly.

If the canvas is blank but the debug panel shows frames arriving, check (a) `bitmap.close()` timing, (b) canvas width/height are being set to the frame resolution.

- [ ] **Step 6: Manual E2E verification — permission denial path.**

- Revoke "Screen Recording" for ahandd in System Settings → Privacy & Security → Screen Recording.
- Restart ahandd.
- In the dashboard, click Start.
- Expected: the state transitions to `error` and the placeholder message contains the permission-denial string from `run_desktop_capture`.

- [ ] **Step 7: Manual E2E verification — memory leak check.**

- Start a session and let it run for **10 minutes** at 10 fps.
- Open Chrome DevTools → Memory → Take heap snapshot.
- Click Stop.
- Take another heap snapshot.
- Diff: `ImageBitmap` count in the "after stop" snapshot should be ≤ 10 (a few stragglers from React state are fine — the count should not have grown by thousands).

If the count exploded, `bitmap.close()` is being missed somewhere — check both the `onFrame` handler in `device-desktop.tsx` AND any early-return paths.

- [ ] **Step 8: Commit.**

```bash
git add apps/hub-dashboard/src/components/device-desktop.tsx apps/hub-dashboard/src/components/device-tabs.tsx
git commit -m "feat(dashboard): DeviceDesktop tab integration with canvas rendering and debug panel"
```

---

## Task 11: Docs — macOS permission docs, spec status, roadmap ticks

**Goal:** Document the macOS Screen Recording permission flow for operators, flip the research spec status to "Phase 1 implemented", and tick every Phase 1 checkbox in the roadmap.

**Files:**
- Modify: `crates/ahandd/README.md` (or whichever doc describes daemon setup; if none exists, create one)
- Modify: `docs/superpowers/specs/2026-04-12-remote-desktop-research.md`
- Modify: `docs/remote-control-roadmap.md`

**Acceptance Criteria:**
- [ ] Daemon README has a "macOS Screen Recording Permission" section describing the first-run flow
- [ ] Spec status line updated to "Phase 1 implemented" with a date
- [ ] All Phase 1 checkboxes in the roadmap Section 4 are checked

**Verify:** Visual inspection of the three files + `grep -c '\[x\]' docs/remote-control-roadmap.md` is at least as large as the Phase 1 checkbox count.

**Steps:**

- [ ] **Step 1: Add macOS permission section.** In `crates/ahandd/README.md` (create it if it doesn't exist):

```markdown
## macOS Screen Recording Permission

Remote desktop capture on macOS requires the operator to grant Screen Recording
access to the `ahandd` binary:

1. Run `ahandd` once. The first desktop capture request will fail with:
   "screen recording permission denied or capture init failed: ... grant access
    in System Settings → Privacy & Security → Screen Recording, then restart
    ahandd".
2. Open **System Settings → Privacy & Security → Screen Recording**.
3. Toggle the entry for `ahandd` on. If there's no entry, click `+` and select
   the binary at `/path/to/ahandd` (or wherever you installed it).
4. Quit and restart `ahandd`. The permission cache refreshes on process start.

After the permission is granted once, subsequent desktop captures work without
additional prompts.
```

- [ ] **Step 2: Update the spec status.** In `docs/superpowers/specs/2026-04-12-remote-desktop-research.md`, near the top:

Change:
```markdown
**Status:** Design locked in, implementation pending.
```
To:
```markdown
**Status:** Phase 1 implemented (YYYY-MM-DD — replace with actual merge date).
```

- [ ] **Step 3: Tick all Phase 1 roadmap checkboxes.** In `docs/remote-control-roadmap.md` Section 4, change every `- [ ]` under `**Phase 1 — macOS view-only prototype (current target)**` to `- [x]`. Leave later phases untouched.

Also change the phase header from "(current target)" to "(implemented)":

```markdown
**Phase 1 — macOS view-only prototype (implemented)**
- [x] Protocol: ...
- [x] Daemon: ...
...
```

- [ ] **Step 4: Visual sanity check.**

Run: Read all three modified files.
Expected: no stray TBDs, no broken markdown formatting, checkboxes are consistent.

- [ ] **Step 5: Commit.**

```bash
git add crates/ahandd/README.md docs/superpowers/specs/2026-04-12-remote-desktop-research.md docs/remote-control-roadmap.md
git commit -m "docs: remote desktop Phase 1 — macOS permission docs, spec status, roadmap ticks"
```

---

## Post-implementation

After all 12 tasks (0–11) are committed and CI is green:

1. **Push the branch** and open a PR targeting the main development branch.
2. **Run the full workspace test suite once more** — `cargo test --workspace && pnpm --filter hub-dashboard test && pnpm --filter hub-dashboard build`.
3. **Verify the 10-minute memory leak check** one more time on a clean build.
4. **Write a short PR description** listing the 12 commits and linking the spec + roadmap + this plan.
5. **Note any Phase 1.5 follow-ups** observed during implementation (input-capture foundation decisions, coordinate normalization stubs, etc.) — add them as new checkboxes under Phase 1.5 in the roadmap before merging.











