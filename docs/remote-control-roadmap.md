# Remote Control Roadmap

**Date:** 2026-04-12
**Status:** Long-term vision. Items marked TODO are not yet implemented.

## Philosophy

aHand is an **agent-first, human-assisted** remote control platform. Cloud-side agents (AI orchestrators) manage devices, while humans can intervene when needed (e.g., taking over a browser for login, approving sensitive operations).

All remote control capabilities are exposed through a unified job/session interface. Both agents and humans can operate in any mode.

## Capability Matrix

### 1. Command Execution (unified)

A single job interface with an `interactive` flag.

| Mode | Transport | Use case |
|------|-----------|----------|
| `interactive: false` (pipe) | Unidirectional: stdout/stderr streamed separately | Non-interactive commands (`ls`, `curl`, `cargo build`). Default for agents. |
| `interactive: true` (pty) | Bidirectional: pty allocated, stdin accepted | Interactive programs (`claude`, `vim`, `python REPL`, install wizards). |

Auto-detection is intentionally not implemented — programs' tty requirements can't be reliably inferred from the command name. Instead:
- Agents default to pipe mode; if the process exits with a tty-related error, retry with `interactive: true`.
- Humans choose mode via a toggle in the dashboard terminal.

**Protocol additions needed for interactive mode:**
- `StdinChunk` — client → device, raw bytes for pty stdin
- `TerminalResize` — client → device, cols/rows
- stdout/stderr merge into a single stream (pty behavior)

**Current status:**
- [x] Pipe mode (non-interactive job execution)
- [x] Dashboard terminal UI (pipe mode)
- [ ] TODO: pty allocation in daemon executor
- [ ] TODO: StdinChunk / TerminalResize protocol messages
- [ ] TODO: Bidirectional WebSocket transport for interactive jobs
- [ ] TODO: xterm.js integration in dashboard terminal
- [ ] TODO: `interactive` flag in job creation API

### 2. Browser Automation

Remote control of a browser instance on the device via Playwright.

| Actor | Interaction |
|-------|-------------|
| Agent | Sends structured browser commands (navigate, click, type, screenshot) |
| Human | Takes over browser for manual actions (e.g., login with MFA), then hands back to agent |

**Current status:**
- [x] BrowserRequest / BrowserResponse protocol messages
- [x] Daemon browser manager (Playwright integration)
- [x] Domain allowlist enforcement
- [x] Hub `POST /api/browser` endpoint with base64 binary response
- [x] Dashboard Browser tab with command-based controls (open, click, fill, snapshot, screenshot, PDF, download, close, custom) and response log with image preview
- [ ] TODO: Live browser view with low-latency interactive control (~100ms).
  - **Goal:** user can watch current browser state in real time and take over for manual login / form filling / debugging. Command-based UI has ~1–2s polling latency which is too slow for typing.
  - **Blocker:** daemon currently spawns `playwright-cli` as a stateless subprocess per command; it doesn't hold a persistent Chrome handle and can't extract the CDP WebSocket URL. `@playwright/cli@0.1.1` has no `--output-cdp-url` flag or daemon mode.
  - **Approach options (need to pick one):**
    - Switch daemon to Playwright Node API via long-lived IPC process — keeps Playwright ecosystem but adds Node/Rust boundary complexity
    - Daemon launches Chrome directly with `--remote-debugging-port` and manages CDP itself — drops playwright-cli dependency
    - Check newer `@playwright/cli` versions for server/daemon mode with exposed CDP
  - **Once CDP is reachable:** new hub WS endpoint `/ws/devices/{id}/cdp` transparently proxies CDP frames between dashboard and daemon. Dashboard uses `Page.startScreencast` for live frames and `Input.dispatchMouseEvent` / `Input.dispatchKeyEvent` for interaction. Canvas-based renderer in the Browser tab.
- [ ] TODO: Human takeover handoff protocol (agent pauses, human operates, agent resumes)
- [ ] TODO: Reduce browser install footprint (currently ~250–450 MB: Node.js v24 + `@playwright/cli` + Chromium).
  - **Short-term (incremental):**
    - Detect system Node.js ≥18 and reuse it; only download the bundled Node when the system lacks one. Expected install drops to ~100 MB on dev machines.
    - Lazy install: trigger `browser-init` automatically on first browser command instead of requiring a manual subcommand. Users who never use browser features pay zero cost.
  - **Long-term (rewrite, coupled with live browser view above):**
    - Evaluate switching from `playwright-cli` to a Rust-native CDP client (`chromiumoxide` or similar). Drops Node.js entirely; only Chrome binary remains. Also unblocks the CDP proxy feature for live browser view, so this rewrite is the likely path to both goals.
    - **Main loss to solve:** Playwright's accessibility snapshot with `@eN` element refs (agent-friendly). Need a selector cache in the daemon: when `snapshot` runs, assign IDs to elements and store their CSS selectors; look up the stored selector on subsequent `click`/`fill` references.
    - Prototype chromiumoxide against current feature set before committing to the rewrite.

### 3. File Operations

List, read, upload, download, and modify files on the device.

| Operation | Description |
|-----------|-------------|
| List | Directory listing with metadata (size, permissions, mtime) |
| Read | Stream file content back to cloud |
| Upload | Push file from cloud to device |
| Modify | Patch file content (append, replace, write) |
| Delete | Remove files (with policy enforcement) |

**Current status:**
- [x] File operation protocol messages — `proto/ahand/v1/file_ops.proto` (14 oneof variants on FileRequest / FileResponse, full FileError taxonomy, helper messages)
- [x] Daemon file handler with policy enforcement — `crates/ahandd/src/file_manager/` (allowlist + denylist + dangerous_paths + STRICT-mode approval, traversal + symlink TOCTOU mitigations)
- [x] Hub HTTP forwarding — `POST /api/devices/{id}/files` correlates the FileRequest/FileResponse pair across the WebSocket gateway with admission control + RAII slot cleanup
- [ ] TODO: Dashboard file browser UI
- [ ] TODO: Agent SDK file operation methods
- [ ] Follow-up: full S3 large-file transfer flow (S3Client + S3Config + `FullWrite.s3_object_key` plumbing in place; the `POST /files/upload-url` route was withdrawn until the bidirectional download-before-forward + upload-after-read flow is wired end-to-end)

### 4. Remote Desktop / Screen Control

Full remote control of the device's desktop environment.

| Actor | Interaction |
|-------|-------------|
| Agent | Structured actions (click coordinates, type text, read screen via OCR/screenshot) |
| Human | Live VNC/RDP-style interactive session |

Detailed research and phased plan: [2026-04-12-remote-desktop-research.md](superpowers/specs/2026-04-12-remote-desktop-research.md) — compares screenshot polling / noVNC / RustDesk-style (libvpx + WebCodecs) approaches, locks in a multi-phase path from macOS JPEG prototype to production VP9 + WebCodecs.

**Phase 1 — macOS view-only prototype (current target)**
- [ ] Protocol: `DesktopCaptureConfig` + `DesktopFrame` messages, `JobRequest.desktop_capture` field, `PolicyState.allow_desktop_capture` field
- [ ] Daemon: `desktop.rs` module with `xcap` capture + `image` JPEG encoding
- [ ] Daemon: policy integration (default deny, explicit opt-in via config)
- [ ] Daemon: macOS Screen Recording permission failure handling + docs
- [ ] Hub: `DesktopFrameBus` (live-only broadcast, no history, no persistence)
- [ ] Hub: `/api/desktop/token` endpoint, `/ws/desktop` gateway
- [ ] Hub: `/api/jobs` rejects concurrent same-device desktop sessions (409)
- [ ] Hub: jobs table `desktop_capture_config` JSONB column + migration
- [ ] Hub: audit records `frame_count` + `total_bytes`
- [ ] Dashboard: `device-desktop.tsx` + canvas + debug panel + stream hook
- [ ] Dashboard: Desktop tab in `device-tabs.tsx`
- [ ] Dashboard: 10-minute memory leak check (ImageBitmap cleanup verified)

**Phase 1.5 — macOS input forwarding**
- [ ] Protocol: `DesktopInputEvent` message (MouseMove / MouseButton / MouseScroll / KeyDown / KeyUp)
- [ ] Protocol: coordinates use normalized [0,1] domain so resolution changes mid-session don't break mapping
- [ ] Daemon: `enigo` crate integration for macOS input injection
- [ ] Daemon: `register_desktop` with input channel (mirror of `register_interactive`)
- [ ] Hub: `/ws/desktop` accepts inbound binary/text frames from dashboard → `DesktopInputEvent` envelopes
- [ ] Dashboard: canvas mouse + keyboard event capture + normalization
- [ ] Dashboard: basic keyboard support (ASCII + modifiers); IME/layout deferred to Phase 3

**Phase 1.9 — macOS multi-monitor picker**
- [ ] Protocol: `DesktopDisplayInfo` request/response messages
- [ ] Daemon: enumerate displays via `xcap`
- [ ] Dashboard: display picker UI above canvas; populate from daemon
- [ ] Dashboard: switching displays restarts the session with new `display_index`

**Phase 2 — Production encoding + Linux X11 + Windows**
- [ ] Protocol: `DesktopCaptureConfig.codec` field, `DesktopCodecNegotiate` message, `DesktopKeyframeRequest` message
- [ ] Daemon: libvpx VP9 encoder (realtime/CBR mode, `rc_dropframe_thresh=25`) — reference [RustDesk `libs/scrap/src/common/vpxcodec.rs`](https://github.com/rustdesk/rustdesk/blob/master/libs/scrap/src/common/vpxcodec.rs)
- [ ] Daemon: Linux X11 capture backend via `xcap` (or RustDesk-style XCB SHM if performance demands it)
- [ ] Daemon: Windows capture backend via `xcap` (or DXGI Desktop Duplication for performance)
- [ ] Dashboard: WebCodecs `VideoDecoder` integration replaces the JPEG + `createImageBitmap` path
- [ ] Dashboard: automatic WS reconnection with backoff (terminate + rejoin on drop)
- [ ] Hub: codec-aware routing (VP9 bytes are still opaque envelope payload, but size assertions / rate limits differ)
- [ ] Acceptance: 30 fps at < 5 Mbps on typical LAN, sub-100ms glass-to-glass latency

**Phase 2W — Generic window-scoped capture (non-browser apps)**
- [ ] Protocol: `DesktopCaptureConfig.target` oneof (display vs window), `WindowRef` message
- [ ] Daemon: window enumeration API — macOS `CGWindowListCopyWindowInfo`, X11 composite extension, Windows `EnumWindows`
- [ ] Daemon: per-window capture — macOS `CGWindowListCreateImage`, X11 `XCompositeNameWindowPixmap`, Windows `PrintWindow`/`BitBlt`
- [ ] Daemon: input forwarding scoped to target window (bring-to-front + window-relative coordinate mapping; macOS may require Accessibility API)
- [ ] Dashboard: window picker UI listing available windows from daemon
- [ ] Docs: clear guidance — for browsers, use the Browser tab (CDP live view from Section 2 of this roadmap); for other apps, use window capture

**Phase 2.5 — Linux Wayland + multi-observer**
- [ ] Daemon: Wayland capture via `xdg-desktop-portal` ScreenCast + PipeWire (with-display path)
- [ ] Daemon: Wayland headless path via `uinput` helper process (separate privileged process)
- [ ] Daemon: Wayland input injection via portal RemoteDesktop or uinput
- [ ] Hub: `DesktopFrameBus` upgraded to support multiple concurrent subscribers per session (one capture, many viewers)
- [ ] Dashboard: show current viewer count in debug panel

**Phase 3 — Quality & hardening**
- [ ] Daemon: adaptive QoS — fps / bitrate / keyframe interval feedback loop based on `DesktopSessionStats` (reference [RustDesk `src/server/video_qos.rs`](https://github.com/rustdesk/rustdesk/blob/master/src/server/video_qos.rs))
- [ ] Daemon: hardware encoding (NVENC / VAAPI / AMF on Linux/Windows; VideoToolbox on macOS) via ffmpeg or `rustdesk-org/hwcodec`
- [ ] Daemon: clipboard sync (new protocol message family, piggybacks on desktop session)
- [ ] Daemon: file drag-and-drop (integrates with File Operations work from roadmap Section 3)
- [ ] Daemon: keyboard layout mapping + IME support (reference RustDesk `libs/enigo/src/macos/macos_impl.rs` for TIS layout-aware translation)
- [ ] Hub: session recording to persistent storage (opt-in, audited)
- [ ] Dashboard: clipboard UI, file drag-drop, recording playback

### 5. Local MCP Execution

Execute MCP (Model Context Protocol) servers installed on the device, allowing cloud agents to use local tools.

| Use case | Example |
|----------|---------|
| Local database access | Agent queries a local Postgres via MCP |
| Local file system tools | Agent uses local filesystem MCP for code exploration |
| Hardware-specific tools | Agent accesses local GPU, sensors, or peripherals via MCP |

**Current status:**
- [ ] TODO: MCP proxy protocol (forward MCP tool calls through aHand tunnel)
- [ ] TODO: Daemon MCP bridge
- [ ] TODO: MCP tool discovery and registration

## Architecture Principles

1. **Unified interface** — All capabilities go through the job/session system. One auth model, one audit trail, one policy engine.
2. **Agent-first** — API design optimized for programmatic use. Human UI is built on top of the same API.
3. **Policy enforcement** — Every action goes through the daemon's policy checker. Allowlists, denylists, approval workflows apply uniformly.
4. **Audit everything** — All remote operations are logged with caller identity, timestamps, and results.
5. **Graceful degradation** — If a capability isn't available (no browser installed, no desktop environment), fail clearly rather than silently.
