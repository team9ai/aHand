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
- [ ] TODO: File operation protocol messages
- [ ] TODO: Daemon file handler with policy enforcement
- [ ] TODO: Dashboard file browser UI
- [ ] TODO: Agent SDK file operation methods

### 4. Remote Desktop / Screen Control

Full remote control of the device's desktop environment.

| Actor | Interaction |
|-------|-------------|
| Agent | Structured actions (click coordinates, type text, read screen via OCR/screenshot) |
| Human | Live VNC/RDP-style interactive session |

**Current status:**
- [ ] TODO: Screen capture protocol
- [ ] TODO: Input event forwarding
- [ ] TODO: Dashboard remote desktop viewer

Detailed research and phased plan: [2026-04-12-remote-desktop-research.md](superpowers/specs/2026-04-12-remote-desktop-research.md) — compares screenshot polling / noVNC / RustDesk-style (libvpx + WebCodecs) approaches, proposes a two-phase prototype → production path.

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
