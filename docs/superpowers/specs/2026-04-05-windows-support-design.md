# aHand Windows Support Design

**Date**: 2026-04-05
**Status**: Draft
**Branch strategy**: From `dev`, `feat/windows-support` as reference only
**Approach**: Conditional compilation (`#[cfg(unix)]` / `#[cfg(windows)]`) with inline platform branches

## Background

aHand currently only runs on Unix (macOS / Linux). The primary blockers for Windows support are:

1. Unix Domain Socket IPC (daemon ↔ CLI communication)
2. Unix signal handling (SIGTERM/SIGINT)
3. Bash shell script dependencies (install, upgrade, browser-init)
4. Unix file permissions (`PermissionsExt`, `0o600`)
5. Unix process management (`kill`, `ps`, `process_group`)

The `feat/windows-support` branch has partial implementations for items 1-2 and 5, but is based on a diverged codebase (hub crates removed, browser migrated to agent-browser). This design starts fresh from `dev`, using that branch as reference only.

## Decision Log

| Topic | Decision | Rationale |
|-------|----------|-----------|
| peer_cred identity | Full Windows API (PID → token → SID → username) | Security parity with Unix |
| install.sh | Bash + PowerShell dual scripts | Users need `curl \| sh` / `irm \| iex` for first install |
| ahandctl install-daemon | New Rust subcommand | Script-free daemon installation capability |
| upgrade.sh | Rust rewrite into ahandctl | Remove bash dependency |
| setup-browser.sh | Rust rewrite into browser_init.rs | Remove bash dependency, already partially done |
| Node.js | Keep (adapt .zip install for Windows) | daemon.js (ncc-bundled playwright-core) requires Node runtime |
| File permissions | ACL via Win32 API for parity | Equivalent to Unix 0o600 |
| Key storage | DPAPI encryption on Windows | Stronger than ACL alone; admin cannot read plaintext |
| Testing | Full: compile + unit + IPC integration + browser-init | Add Windows CI runner |
| Architecture | Conditional compilation, no trait abstraction | Only 2 platforms, minimal abstraction overhead |

## Section 1: IPC Layer

### Server (ahandd/src/ipc.rs)

`serve_ipc` keeps its unified public signature. Internally dispatches by platform:

- **Unix**: `UnixListener::bind` → `stream.peer_cred()` → `"uid:{uid}"`
- **Windows**: `ServerOptions::new().first_pipe_instance(true).create(pipe_name)` → accept loop creating new `ServerOptions` per connection → `GetNamedPipeClientProcessId` → `OpenProcess` → `OpenProcessToken` → `GetTokenInformation(TokenUser)` → `LookupAccountSidW` → `"user:{username}"`

Key design points:

- **Generic `handle_ipc_conn<R: AsyncRead, W: AsyncWrite>`**: Decouples from `UnixStream`. The frame protocol (`read_frame`/`write_frame`) already operates on generic `AsyncRead`/`AsyncWrite`.
- **`spawn_ipc_handler` helper**: Wraps `tokio::spawn` + type erasure to prevent generic propagation to callers.
- **Named Pipe accept pattern**: Unlike Unix sockets where `accept()` returns a new stream, Named Pipes require creating a fresh `ServerOptions` instance after each `connect()`.

### Client (ahandctl/src/main.rs)

Extract `ipc_connect()` function. All 5 call sites (`ipc_exec`, `ipc_cancel`, `ipc_approve`, `ipc_policy`, `ipc_session`) replace `UnixStream::connect` with this unified function:

```rust
#[cfg(unix)]
async fn ipc_connect(path: &str) -> Result<(impl AsyncRead + Unpin, impl AsyncWrite + Unpin)> {
    let stream = tokio::net::UnixStream::connect(path).await?;
    Ok(stream.into_split())
}

#[cfg(windows)]
async fn ipc_connect(path: &str) -> Result<(impl AsyncRead + Unpin, impl AsyncWrite + Unpin)> {
    let client = tokio::net::windows::named_pipe::ClientOptions::new().open(path)?;
    Ok(tokio::io::split(client))
}
```

### Socket Path (config.rs)

`ipc_socket_path()` returns platform-specific defaults:

- Unix: `~/.ahand/ahandd.sock`
- Windows: `\\.\pipe\ahandd`

Configurable via `ipc_socket_path` in config.toml or `--ipc-socket` CLI arg on both platforms.

## Section 2: Signal Handling & Process Management

### Signal Handling (ahandd/src/main.rs)

Graceful daemon shutdown:

- **Unix**: `tokio::signal::unix::signal(SignalKind::terminate())` + `SignalKind::interrupt()`
- **Windows**: `tokio::signal::ctrl_c()`

Wrapped in `#[cfg]` blocks around the `tokio::select!` shutdown race.

### Process Management (ahandctl/src/daemon.rs)

Three functions need platform branches:

| Function | Unix | Windows |
|----------|------|---------|
| `is_process_running` | Linux: `/proc/{pid}` exists; macOS: `ps -p {pid}` | `tasklist /FI "PID eq {pid}" /NH` → check output |
| `send_signal` | `kill -{sig} {pid}` | `taskkill /PID {pid} /F` |
| `start` (daemon background launch) | `CommandExt::process_group(0)` | `CommandExt::creation_flags(CREATE_NO_WINDOW \| DETACHED_PROCESS)` |

## Section 3: File Permission Security Model

### Cross-Platform Permission Module (ahandd/src/fs_perms.rs)

Two public functions consolidating all permission operations:

```rust
/// Equivalent to Unix 0o600 — only current user can read/write.
pub fn restrict_owner_only(path: &Path) -> io::Result<()>;

/// Equivalent to Unix 0o660 — current user + group can read/write.
pub fn restrict_owner_and_group(path: &Path) -> io::Result<()>;
```

**Unix implementation**: `PermissionsExt::from_mode()`.

**Windows implementation**:
1. `GetNamedSecurityInfoW` to get current DACL
2. Build new ACL with only current user SID having `GENERIC_READ | GENERIC_WRITE`
3. `SetNamedSecurityInfoW` to apply
4. Uses `windows-sys` crate (Microsoft-maintained)

For `restrict_owner_and_group`: adds `BUILTIN\Users` group ACE on Windows.

IPC Named Pipe permissions are handled separately via `ServerOptions` security attributes, not this module.

### Call sites migrated to fs_perms:

- `ipc.rs` — socket file permissions (Unix only, Windows uses pipe security)
- `openclaw/device_identity.rs` — Ed25519 private key
- `openclaw/exec_approvals.rs` — approval records
- `openclaw/pairing.rs` — pairing key

### DPAPI Key Encryption (ahandd/src/dpapi.rs, Windows only)

For Ed25519 private keys in `openclaw/device_identity.rs`:

- **Write**: plaintext → `CryptProtectData` → ciphertext written to disk
- **Read**: ciphertext from disk → `CryptUnprotectData` → plaintext

Transparent to the user. Bound to current Windows user account — even admin cannot decrypt on another account. If user resets their Windows account, device identity is regenerated (correct behavior).

Unix side unchanged (0o600 file permissions only).

## Section 4: Scripts & Installation

### install.ps1 (new)

PowerShell equivalent of `install.sh` for Windows users:

```powershell
# Usage: irm https://xxx/install.ps1 | iex
```

- Detect platform (x64/arm64)
- Download `ahandd-windows-x64.exe` + `ahandctl-windows-x64.exe` from GitHub Releases
- Verify SHA-256 checksums
- Place in `~/.ahand/bin/`
- Add `~/.ahand/bin` to user PATH via `[Environment]::SetEnvironmentVariable`

Bash `install.sh` unchanged.

### ahandctl install-daemon (new subcommand)

Rust-native daemon installation embedded in `ahandctl`:

- Reuses `download_bytes`, `platform_info` helpers from `browser_init.rs`
- Downloads `ahandd` binary from GitHub Releases, verifies checksum, places in `~/.ahand/bin/`
- Cross-platform (Unix + Windows)
- Use case: user has `ahandctl` (e.g., via cargo install) and needs to install daemon without scripts

### upgrade (Rust rewrite)

Migrate `upgrade.sh` logic entirely into `ahandctl/src/upgrade.rs`:

- Query current version → GitHub API latest release → download binaries + admin SPA → verify checksum → stop daemon → replace files
- Remove `Command::new("bash")` call
- Windows special handling: running `.exe` cannot be overwritten directly — stop daemon first, then replace (or rename-then-replace strategy)

### browser-init (complete Windows adaptation)

`browser_init.rs` already has Rust implementation base. Gaps to fill:

- **Node.js install**: Windows downloads `.zip` format, use `zip` crate to extract (vs `tar` + `xz2` on Unix)
- **`detect_system_chrome`**: Add `#[cfg(windows)]` branch checking `Program Files/Google/Chrome/Application/chrome.exe` and Microsoft Edge paths
- **Clean logic**: `pkill` → `taskkill` (reference branch has this)

## Section 5: CI & Testing

### Build Matrix (release-rust.yml)

Add to existing Linux/macOS targets:

```yaml
- os: windows-latest
  target: x86_64-pc-windows-msvc
  suffix: windows-x64
```

- Install protoc via `arduino/setup-protoc@v3`
- Output `.exe` suffix artifacts
- Use `sha256sum` for checksums
- Skip macOS universal binary step with `if: runner.os != 'Windows'`

### Test CI (new or extended workflow)

```yaml
test-windows:
  runs-on: windows-latest
  steps:
    - cargo test -p ahand-protocol
    - cargo test -p ahandd
    - cargo test -p ahandctl
```

### New Tests

**IPC integration (ahandd/tests/ipc_roundtrip.rs)**:
- Start `serve_ipc`, connect via `ipc_connect`
- Send `JobRequest` → receive `JobFinished`
- Send `CancelJob` → verify cancellation
- Send `SessionQuery` → receive `SessionState`
- Same test code on both platforms via `ipc_connect` abstraction

**Peer identity**:
- Unix: verify `caller_uid` format `"uid:{number}"`
- Windows: verify format `"user:{username}"`

**File permissions (ahandd/tests/fs_perms_test.rs)**:
- `restrict_owner_only`: verify other users cannot read (Unix: `stat` mode check; Windows: `GetNamedSecurityInfoW` DACL check)

**DPAPI (ahandd/tests/dpapi_test.rs, Windows only)**:
- Encrypt → decrypt roundtrip
- Verify ciphertext differs from plaintext

**browser-init**:
- Node.js download: Unix `.tar.xz`, Windows `.zip` extraction
- `detect_system_chrome`: mock path detection per platform

**install/upgrade**:
- `ahandctl install-daemon`: mock HTTP download → verify binary placement + checksum
- `ahandctl upgrade`: mock version query + download → verify replacement flow

### Distribution Script Tests (test-dist-scripts.yml)

Extend matrix to include `windows-latest` for `install.ps1` testing.

## Section 6: Dependencies & Cargo Configuration

### New Dependencies

```toml
# crates/ahandd/Cargo.toml
[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.59", features = [
    "Win32_Security",
    "Win32_Security_Authorization",
    "Win32_Security_Cryptography",    # DPAPI
    "Win32_System_Pipes",
    "Win32_System_Threading",
] }
zip = "2"
```

`windows-sys` is Microsoft's official zero-overhead FFI bindings (pure type definitions + extern). Conditional dependency via `cfg(windows)` — Unix compilation unaffected.

### No Changes Needed

- `tokio = { features = ["full"] }` already includes Named Pipe support
- No need for `interprocess` crate — tokio native Named Pipe is sufficient
- No need for `signal-hook` — tokio built-in signal handling is sufficient
- `ahandctl` needs no new dependencies — client-side Named Pipe is in `tokio::net::windows::named_pipe`

## Section 7: File Change Summary

### New Files

| File | Content |
|------|---------|
| `crates/ahandd/src/fs_perms.rs` | Cross-platform file permission module |
| `crates/ahandd/src/dpapi.rs` | Windows DPAPI encrypt/decrypt wrapper (`#[cfg(windows)]`) |
| `crates/ahandd/tests/ipc_roundtrip.rs` | IPC integration tests |
| `crates/ahandd/tests/fs_perms_test.rs` | File permission tests |
| `crates/ahandd/tests/dpapi_test.rs` | DPAPI roundtrip tests (Windows only) |
| `scripts/dist/install.ps1` | PowerShell installation script |

### Modified Files

| File | Changes |
|------|---------|
| `crates/ahandd/src/ipc.rs` | Generic `handle_ipc_conn`, add `serve_ipc_unix` / `serve_ipc_windows`, Windows peer identity |
| `crates/ahandd/src/main.rs` | Signal handling `#[cfg]` branches |
| `crates/ahandd/src/config.rs` | `ipc_socket_path()` Windows default Named Pipe path |
| `crates/ahandd/src/browser_init.rs` | Node.js `.zip` install, `detect_system_chrome` Windows branch, `clean` with taskkill |
| `crates/ahandd/src/openclaw/device_identity.rs` | Key read/write via `fs_perms` + DPAPI |
| `crates/ahandd/src/openclaw/exec_approvals.rs` | Permission setting via `fs_perms` |
| `crates/ahandd/src/openclaw/pairing.rs` | Permission setting via `fs_perms` |
| `crates/ahandd/Cargo.toml` | Add `windows-sys`, `zip` conditional dependencies |
| `crates/ahandctl/src/main.rs` | Extract `ipc_connect()`, replace 5x `UnixStream::connect` |
| `crates/ahandctl/src/daemon.rs` | `is_process_running` / `send_signal` / `start` Windows branches |
| `crates/ahandctl/src/upgrade.rs` | Rust rewrite, remove bash invocation |
| `crates/ahandctl/src/admin.rs` | Replace `Command::new("bash")` browser setup call with direct invocation of `browser_init::run()` |
| `.github/workflows/release-rust.yml` | Add Windows build target |
| `.github/workflows/test-rust.yml` (new or extended) | Windows test runner |

### Unchanged

- `ahand-protocol` — pure protobuf, already cross-platform
- `ahand-hub*` — server-side, out of scope
- `install.sh` — kept for Unix users
- `upgrade.sh` — kept until Rust rewrite is complete, then deprecated
