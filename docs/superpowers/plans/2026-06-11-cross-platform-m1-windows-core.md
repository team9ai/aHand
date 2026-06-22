# M1 — Windows Core Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** ahandd/ahandctl compile, test, and run natively on Windows: cross-platform IPC, process management, signals, shell resolution, path/policy fixes, secure files, updater, and a Windows CI lane.

**Architecture:** New shared workspace crate `ahand-platform` owns all OS differences (ipc transport, process lifecycle, shell, secure files, path normalization, shutdown signals). `ahandd`/`ahandctl` call it; scattered `#[cfg]` outside the crate is limited to trivial one-liners. See spec: `docs/superpowers/specs/2026-06-11-cross-platform-windows-linux-design.md`.

**Tech Stack:** Rust 2024 workspace, tokio (named pipes need `features=["full"]`, already on), `dunce` (verbatim-prefix stripping), `libc` (Unix kill), `windows`-targeted code verified via `cargo check --target x86_64-pc-windows-msvc` on the dev Mac, real test runs on `windows-latest` CI.

**Dev-loop note (IMPORTANT):** the dev machine is macOS. Windows code cannot *run* locally, but it MUST compile: every task's verify includes `cargo check --target x86_64-pc-windows-msvc`. Windows-only unit tests run in CI (Task 10). Do not skip the cross-check.

**Conventions:** edition 2024, `anyhow::Result` + `.context()`, tracing macros, tests colocated `#[cfg(test)]` or in `crates/<c>/tests/`. After touching shared crates run `cargo check --workspace`.

---

## File Structure

```
crates/ahand-platform/            # NEW shared crate
├── Cargo.toml
└── src/
    ├── lib.rs                    # pub mod paths; process; shell; secure_file; signals; ipc
    ├── paths.rs                  # exe_name(), simplify(), display path helpers
    ├── process.rs                # configure_detached(), terminate(), is_process_running()
    ├── shell.rs                  # default_shell(), env_shell(), shell_c_flag()
    ├── secure_file.rs            # write_secure_file(), restrict_to_owner()
    ├── signals.rs                # shutdown_signal()
    └── ipc.rs                    # IpcEndpoint, IpcListener, ipc_connect()

Modified:
Cargo.toml                        # workspace members + [workspace.dependencies] additions
.gitattributes                    # NEW: LF enforcement
crates/ahandd/Cargo.toml          # + ahand-platform, dunce
crates/ahandd/src/{main,ipc,config,executor,updater,device_identity}.rs
crates/ahandd/src/file_manager/policy.rs
crates/ahandd/src/openclaw/{handler,device_identity,pairing}.rs
crates/ahandd/src/browser.rs      # /tmp fallback only (PATH ':' fix is M3)
crates/ahandctl/Cargo.toml        # + ahand-platform
crates/ahandctl/src/{main,daemon}.rs
.github/workflows/client-ci.yml   # NEW: 3-OS test lane
.github/workflows/release-rust.yml# windows-x64 target + .exe artifacts
```

Task order: 1 → (2,3,4,5,6 in any order) → 7 → 8 → 9 → 10.

---

### Task 1: Scaffold `ahand-platform` crate, `paths` module, `.gitattributes`

**Goal:** Workspace builds with the new crate; path helpers (exe suffix, verbatim stripping) exist and are tested; repo is CRLF-safe.

**Files:**
- Create: `crates/ahand-platform/Cargo.toml`, `crates/ahand-platform/src/lib.rs`, `crates/ahand-platform/src/paths.rs`, `.gitattributes`
- Modify: `Cargo.toml` (workspace members, line 3-9; workspace.dependencies)

**Acceptance Criteria:**
- [ ] `cargo test -p ahand-platform` passes on macOS
- [ ] `cargo check --workspace` passes
- [ ] `cargo check --target x86_64-pc-windows-msvc -p ahand-platform` passes
- [ ] `.gitattributes` forces LF for `*.sh`/`*.bats`

**Verify:** `cargo test -p ahand-platform && cargo check --workspace && cargo check --target x86_64-pc-windows-msvc -p ahand-platform`

**Steps:**

- [ ] **Step 1: One-time toolchain setup**

Run: `rustup target add x86_64-pc-windows-msvc`
Expected: target installed (idempotent if already present).

- [ ] **Step 2: Create `.gitattributes` at repo root**

```gitattributes
* text=auto
*.sh text eol=lf
*.bats text eol=lf
*.rs text eol=lf
*.toml text eol=lf
*.proto text eol=lf
```

- [ ] **Step 3: Add crate to workspace**

In root `Cargo.toml`, add `"crates/ahand-platform"` to `members` (keep list style):

```toml
members = [
    "crates/ahand-platform",
    "crates/ahand-protocol", "crates/ahandctl",
    "crates/ahandd",
    "crates/ahand-hub-core",
    "crates/ahand-hub-store",
    "crates/ahand-hub",
]
```

Append to `[workspace.dependencies]`:

```toml
dunce = "1"
```

- [ ] **Step 4: Create `crates/ahand-platform/Cargo.toml`**

```toml
[package]
name = "ahand-platform"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
tokio.workspace = true
anyhow.workspace = true
tracing.workspace = true
dunce.workspace = true

[target.'cfg(unix)'.dependencies]
libc = "0.2"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 5: Create `src/lib.rs`**

```rust
//! Platform abstraction layer for aHand client binaries (`ahandd`, `ahandctl`).
//!
//! All OS-conditional behavior lives here so the rest of the codebase stays
//! `#[cfg]`-free. Each module documents the Unix and Windows semantics it
//! guarantees; anything it cannot make equivalent is documented at the call
//! site it serves.

pub mod paths;
```

(Other `pub mod`s are added by their own tasks.)

- [ ] **Step 6: Write failing tests for `paths` in `src/paths.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_name_appends_exe_only_on_windows() {
        let n = exe_name("ahandd");
        #[cfg(windows)]
        assert_eq!(n, "ahandd.exe");
        #[cfg(not(windows))]
        assert_eq!(n, "ahandd");
    }

    #[test]
    fn simplify_is_identity_for_plain_paths() {
        let p = std::path::Path::new("/a/b");
        assert_eq!(simplify(p), std::path::PathBuf::from("/a/b"));
    }

    #[test]
    fn simplify_strips_verbatim_prefix_on_windows() {
        // On Unix this is a no-op path with backslashes in the file name —
        // only assert the Windows behavior under cfg(windows).
        #[cfg(windows)]
        {
            let p = std::path::Path::new(r"\\?\C:\Users\x");
            assert_eq!(simplify(p), std::path::PathBuf::from(r"C:\Users\x"));
        }
    }

    #[test]
    fn canonicalize_simplified_has_no_verbatim_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let c = canonicalize_simplified(tmp.path()).unwrap();
        assert!(!c.to_string_lossy().starts_with(r"\\?\"));
        assert!(c.is_absolute());
    }
}
```

- [ ] **Step 7: Run tests to verify they fail**

Run: `cargo test -p ahand-platform`
Expected: compile FAIL — `exe_name`, `simplify`, `canonicalize_simplified` not defined.

- [ ] **Step 8: Implement `src/paths.rs` (above the test module)**

```rust
//! Path helpers that make Windows paths behave like Unix paths for the rest
//! of the codebase: executable naming and verbatim-prefix (`\\?\`) stripping.

use std::io;
use std::path::{Path, PathBuf};

/// Append the platform executable suffix (`.exe` on Windows).
pub fn exe_name(base: &str) -> String {
    #[cfg(windows)]
    {
        format!("{base}.exe")
    }
    #[cfg(not(windows))]
    {
        base.to_string()
    }
}

/// Strip Windows verbatim prefixes (`\\?\`, `\\?\UNC\`) so the result is
/// comparable with user-written config patterns. Identity on Unix.
pub fn simplify(path: &Path) -> PathBuf {
    dunce::simplified(path).to_path_buf()
}

/// `std::fs::canonicalize` + [`simplify`]. Use this INSTEAD of raw
/// `canonicalize` anywhere the result is string-compared or glob-matched
/// (policy allow/deny lists), or shown to users.
pub fn canonicalize_simplified(path: &Path) -> io::Result<PathBuf> {
    Ok(simplify(&std::fs::canonicalize(path)?))
}
```

- [ ] **Step 9: Verify**

Run: `cargo test -p ahand-platform && cargo check --workspace && cargo check --target x86_64-pc-windows-msvc -p ahand-platform`
Expected: all PASS.

- [ ] **Step 10: Commit**

```bash
git add Cargo.toml Cargo.lock .gitattributes crates/ahand-platform
git commit -m "feat(platform): scaffold ahand-platform crate with path helpers"
```

---

### Task 2: `platform::process` + adopt in `ahandctl` daemon lifecycle

**Goal:** Detached spawn, terminate, and liveness checks work on Unix and Windows through one module; `ahandctl start/stop/status` no longer uses Unix-only `kill`/`process_group` directly.

**Files:**
- Create: `crates/ahand-platform/src/process.rs`
- Modify: `crates/ahand-platform/src/lib.rs` (add `pub mod process;`)
- Modify: `crates/ahandctl/Cargo.toml` (add `ahand-platform = { path = "../ahand-platform" }`)
- Modify: `crates/ahandctl/src/daemon.rs`

**Acceptance Criteria:**
- [ ] `is_process_running`, `terminate`, `configure_detached` live in `ahand-platform` with per-OS impls
- [ ] `ahandctl/src/daemon.rs` has NO `#[cfg]` blocks and no `kill`/`process_group` references
- [ ] Unix terminate uses `libc::kill` (no subprocess); Windows uses `taskkill`
- [ ] `cargo test -p ahand-platform -p ahandctl` and msvc check pass

**Verify:** `cargo test -p ahand-platform -p ahandctl && cargo check --target x86_64-pc-windows-msvc -p ahandctl`

**Steps:**

- [ ] **Step 1: Write failing tests in `src/process.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_running() {
        assert!(is_process_running(std::process::id()));
    }

    #[test]
    fn nonexistent_pid_is_not_running() {
        // PID near the top of the range; collision chance negligible.
        assert!(!is_process_running(u32::MAX - 7));
    }

    #[test]
    fn terminate_kills_a_spawned_child() {
        let mut cmd = std::process::Command::new(sleep_cmd());
        sleep_args(&mut cmd);
        let child = cmd.spawn().expect("spawn sleeper");
        let pid = child.id();
        assert!(is_process_running(pid));
        terminate(pid, TerminateMode::Force).expect("terminate");
        // Allow the OS a moment to reap.
        for _ in 0..50 {
            if !is_process_running(pid) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("process {pid} still running after terminate");
    }

    fn sleep_cmd() -> &'static str {
        if cfg!(windows) { "cmd.exe" } else { "sleep" }
    }

    fn sleep_args(cmd: &mut std::process::Command) {
        if cfg!(windows) {
            cmd.args(["/C", "ping -n 60 127.0.0.1 >NUL"]);
        } else {
            cmd.arg("60");
        }
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ahand-platform process`
Expected: compile FAIL — module/functions not defined.

- [ ] **Step 3: Implement `src/process.rs`**

```rust
//! Cross-platform process lifecycle: detached spawn, liveness, termination.
//!
//! Windows has no SIGTERM. `TerminateMode::Graceful` is therefore only a
//! *request* level on Unix (SIGTERM); on Windows both modes hard-kill via
//! `taskkill` (`/F` for Force). Callers that need graceful shutdown on
//! Windows must use an application-level channel (e.g. IPC shutdown message).

use anyhow::{Context, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminateMode {
    /// SIGTERM on Unix; `taskkill /PID n` (no /F) on Windows.
    Graceful,
    /// SIGKILL on Unix; `taskkill /F /PID n` on Windows.
    Force,
}

/// Configure a command to run detached from the current terminal/console so
/// it survives the parent exiting (new process group on Unix; detached,
/// windowless process on Windows).
pub fn configure_detached(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
}

#[cfg(unix)]
pub fn is_process_running(pid: u32) -> bool {
    // kill(pid, 0) probes existence without signaling. EPERM means it exists
    // but belongs to another user.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
pub fn is_process_running(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|output| {
            let stdout = String::from_utf8_lossy(&output.stdout);
            output.status.success()
                && stdout
                    .split_whitespace()
                    .any(|w| w == pid.to_string().as_str())
        })
        .unwrap_or(false)
}

#[cfg(unix)]
pub fn terminate(pid: u32, mode: TerminateMode) -> Result<()> {
    let sig = match mode {
        TerminateMode::Graceful => libc::SIGTERM,
        TerminateMode::Force => libc::SIGKILL,
    };
    let r = unsafe { libc::kill(pid as libc::pid_t, sig) };
    if r != 0 {
        let e = std::io::Error::last_os_error();
        // ESRCH: already gone — that's success for our purposes.
        if e.raw_os_error() != Some(libc::ESRCH) {
            return Err(e).context(format!("kill({pid})"));
        }
    }
    Ok(())
}

#[cfg(windows)]
pub fn terminate(pid: u32, mode: TerminateMode) -> Result<()> {
    let mut cmd = std::process::Command::new("taskkill");
    if matches!(mode, TerminateMode::Force) {
        cmd.arg("/F");
    }
    let output = cmd
        .args(["/PID", &pid.to_string()])
        .output()
        .context("failed to run taskkill")?;
    if !output.status.success() {
        let msg = String::from_utf8_lossy(&output.stderr);
        // "not found" (process already gone) counts as success.
        if !msg.contains("not found") && !msg.contains("128") {
            anyhow::bail!("taskkill /PID {pid} failed: {msg}");
        }
    }
    Ok(())
}
```

Add `pub mod process;` to `lib.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p ahand-platform process`
Expected: PASS (3 tests).

- [ ] **Step 5: Adopt in `ahandctl/src/daemon.rs`**

Add dep in `crates/ahandctl/Cargo.toml` under `[dependencies]`:

```toml
ahand-platform = { path = "../ahand-platform" }
```

In `daemon.rs`:

1. DELETE the three `is_process_running` cfg variants (lines 63-93) and `send_signal` (lines 95-104).
2. Add `use ahand_platform::process::{self, TerminateMode};` at the top.
3. `read_running_pid()`: replace `is_process_running(pid)` with `process::is_process_running(pid)`.
4. `find_ahandd_binary()`: replace both literal `"ahandd"` joins with `ahand_platform::paths::exe_name("ahandd")`:

```rust
fn find_ahandd_binary() -> Result<PathBuf> {
    let bin = ahand_platform::paths::exe_name("ahandd");
    // 1. Installed location: ~/.ahand/bin/ahandd[.exe]
    if let Some(home) = dirs::home_dir() {
        let installed = home.join(".ahand").join("bin").join(&bin);
        if installed.exists() {
            return Ok(installed);
        }
    }
    // 2. Sibling of current executable (dev builds: target/debug/)
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let sibling = dir.join(&bin);
            if sibling.exists() {
                return Ok(sibling);
            }
        }
    }
    anyhow::bail!("Cannot find ahandd binary. Expected at ~/.ahand/bin/{bin} or next to ahandctl.")
}
```

5. `start()`: replace the `#[cfg(unix)] {{ use ...CommandExt; cmd.process_group(0); }}` block (lines 135-140) with:

```rust
    // Detach so the daemon survives terminal/console close.
    process::configure_detached(&mut cmd);
```

6. `stop()`: replace `send_signal(pid, "-TERM")` with `process::terminate(pid, TerminateMode::Graceful)` and `send_signal(pid, "-KILL")` with `process::terminate(pid, TerminateMode::Force)`; update the two `eprintln!` messages to say "terminate" instead of SIGTERM/SIGKILL (e.g. `"Failed to request stop: {e}"` / `"Daemon did not stop within 10s, force-killing..."`).
7. Replace remaining `is_process_running(` calls in `start()` with `process::is_process_running(`.

- [ ] **Step 6: Verify**

Run: `cargo test -p ahand-platform -p ahandctl && cargo check --target x86_64-pc-windows-msvc -p ahandctl && cargo check --workspace`
Expected: PASS. Also grep-verify: `grep -n "cfg(" crates/ahandctl/src/daemon.rs` → no matches.

- [ ] **Step 7: Commit**

```bash
git add crates/ahand-platform crates/ahandctl
git commit -m "feat(platform): cross-platform process lifecycle; adopt in ahandctl daemon"
```

---

### Task 3: `platform::signals` + adopt in `ahandd` main

**Goal:** Daemon shutdown works on Windows (Ctrl-C / console close) through one `shutdown_signal()` API; `main.rs` loses its `tokio::signal::unix` import.

**Files:**
- Create: `crates/ahand-platform/src/signals.rs`
- Modify: `crates/ahand-platform/src/lib.rs`, `crates/ahandd/Cargo.toml` (add `ahand-platform`), `crates/ahandd/src/main.rs:26,307-309,420-433`

**Acceptance Criteria:**
- [ ] `shutdown_signal()` returns `anyhow::Result<impl Future<Output = &'static str>>`; Unix = SIGTERM|SIGINT, Windows = Ctrl-C
- [ ] `crates/ahandd/src/main.rs` contains no `tokio::signal::unix`
- [ ] daemon shuts down cleanly on SIGTERM on macOS (manual smoke in Step 6)

**Verify:** `cargo test -p ahand-platform && cargo build -p ahandd && cargo check --target x86_64-pc-windows-msvc -p ahandd`

**Steps:**

- [ ] **Step 1: Implement `src/signals.rs` (no useful unit test exists for signal delivery in-process across platforms; covered by the smoke test in Step 6 and daemon e2e later)**

```rust
//! Unified shutdown signal: SIGTERM/SIGINT on Unix, Ctrl-C (incl. console
//! close) on Windows. Returns the *name* of the signal that fired, for logs.

use anyhow::{Context, Result};
use std::future::Future;

#[cfg(unix)]
pub fn shutdown_signal() -> Result<impl Future<Output = &'static str>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    let mut int = signal(SignalKind::interrupt()).context("install SIGINT handler")?;
    Ok(async move {
        tokio::select! {
            _ = term.recv() => "SIGTERM",
            _ = int.recv() => "SIGINT",
        }
    })
}

#[cfg(windows)]
pub fn shutdown_signal() -> Result<impl Future<Output = &'static str>> {
    Ok(async {
        // ctrl_c covers Ctrl-C, Ctrl-Break delivery for console processes.
        let _ = tokio::signal::ctrl_c().await;
        "ctrl-c"
    })
}
```

Add `pub mod signals;` to `lib.rs`. Add to `crates/ahandd/Cargo.toml` `[dependencies]`:

```toml
ahand-platform = { path = "../ahand-platform" }
```

- [ ] **Step 2: Adopt in `ahandd/src/main.rs`**

1. Line 26: DELETE `use tokio::signal::unix::{SignalKind, signal};`
2. Lines 307-309 (`let mut sigterm = ...; let mut sigint = ...;`) become:

```rust
    // Set up signal handlers for graceful shutdown (SIGTERM/SIGINT on Unix,
    // Ctrl-C on Windows).
    let shutdown = ahand_platform::signals::shutdown_signal()?;
```

3. Lines 420-433 (`tokio::select!` with two signal arms) become:

```rust
    // Race main event loop against shutdown signals.
    let result = tokio::select! {
        r = main_future => r,
        sig = shutdown => {
            info!(signal = sig, "received shutdown signal, shutting down");
            Ok(())
        }
    };
```

- [ ] **Step 3: Build**

Run: `cargo build -p ahandd && cargo check --target x86_64-pc-windows-msvc -p ahandd`
Expected: PASS. (The msvc check will surface OTHER pre-existing Windows breakage in ahandd — `ipc.rs`, `updater.rs`, openclaw — those are later tasks. If the check fails ONLY in files owned by later tasks, record that in the task report and move on; the final gate is Task 10.)

NOTE: if `cargo check --target x86_64-pc-windows-msvc -p ahandd` cannot pass until Tasks 4-8 land, that is EXPECTED — run it, save the error list, and confirm every error belongs to a later task's file set.

- [ ] **Step 4: Run existing tests**

Run: `cargo test -p ahandd --test hub_handshake && cargo test -p ahand-platform`
Expected: PASS.

- [ ] **Step 5: grep gate**

Run: `grep -rn "signal::unix" crates/ahandd/src/`
Expected: no matches.

- [ ] **Step 6: Manual smoke (macOS)**

Run: `cargo build -p ahandd && (target/debug/ahandd --help >/dev/null && echo OK)`
Expected: `OK` (binary still functions; full signal smoke happens in daemon e2e).

- [ ] **Step 7: Commit**

```bash
git add crates/ahand-platform crates/ahandd
git commit -m "feat(platform): unified shutdown_signal; drop unix-only signal handling in ahandd"
```

---

### Task 4: `platform::shell` + adopt in executor and openclaw handler

**Goal:** Shell jobs run with `/bin/sh`-equivalent semantics on Windows (`COMSPEC`/cmd.exe), with the existing Unix behavior byte-for-byte unchanged.

**Files:**
- Create: `crates/ahand-platform/src/shell.rs`
- Modify: `crates/ahand-platform/src/lib.rs`, `crates/ahandd/src/executor.rs:65-77,104,301,540-583`, `crates/ahandd/src/openclaw/handler.rs:260-261`

**Acceptance Criteria:**
- [ ] Unix: sentinel resolves `SHELL` → `/bin/sh` fallback with `-l`; Windows: `COMSPEC` → `cmd.exe` fallback with no leading args
- [ ] openclaw exec path uses `sh -c` on Unix / `%COMSPEC% /C` on Windows
- [ ] Existing `tool_resolution_tests` updated and passing; new Windows expectations cfg-gated

**Verify:** `cargo test -p ahandd executor && cargo test -p ahand-platform shell && cargo check --target x86_64-pc-windows-msvc -p ahand-platform`

**Steps:**

- [ ] **Step 1: Write failing tests in `src/shell.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shell_matches_platform() {
        let s = default_shell();
        #[cfg(unix)]
        {
            assert_eq!(s.path, "/bin/sh");
            assert_eq!(s.login_args, vec!["-l".to_string()]);
        }
        #[cfg(windows)]
        {
            assert_eq!(s.path, "cmd.exe");
            assert!(s.login_args.is_empty());
        }
    }

    #[test]
    fn shell_c_flag_matches_platform() {
        #[cfg(unix)]
        assert_eq!(shell_c_flag(), "-c");
        #[cfg(windows)]
        assert_eq!(shell_c_flag(), "/C");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ahand-platform shell`
Expected: compile FAIL.

- [ ] **Step 3: Implement `src/shell.rs`**

```rust
//! Shell resolution. Unix: `$SHELL`, fallback `/bin/sh`, login flag `-l`.
//! Windows: `%COMSPEC%`, fallback `cmd.exe`, no login concept.
//!
//! NOTE: callers send shell *arguments* over the protocol (e.g. `-c <cmd>`),
//! which are inherently platform-flavored; M1 does not translate them. The
//! daemon only guarantees the shell BINARY resolves per-platform.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellSpec {
    pub path: String,
    /// Args injected before user args when the "shell"/"$SHELL" sentinel is
    /// used (`-l` on Unix; empty on Windows).
    pub login_args: Vec<String>,
}

/// The platform fallback shell (ignores environment).
pub fn default_shell() -> ShellSpec {
    #[cfg(unix)]
    {
        ShellSpec { path: "/bin/sh".to_string(), login_args: vec!["-l".to_string()] }
    }
    #[cfg(windows)]
    {
        ShellSpec { path: "cmd.exe".to_string(), login_args: Vec::new() }
    }
}

/// The user's configured shell from the environment (`SHELL` / `COMSPEC`),
/// or `None` if unset.
pub fn env_shell() -> Option<String> {
    #[cfg(unix)]
    {
        std::env::var("SHELL").ok()
    }
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").ok()
    }
}

/// The "run this command string" flag: `-c` (Unix) / `/C` (Windows).
pub fn shell_c_flag() -> &'static str {
    if cfg!(windows) { "/C" } else { "-c" }
}
```

Add `pub mod shell;` to `lib.rs`. Run: `cargo test -p ahand-platform shell` → PASS.

- [ ] **Step 4: Adopt in `executor.rs`**

Replace `resolve_tool` (lines 65-77) with:

```rust
pub fn resolve_tool(tool: &str, shell_env: Option<&str>) -> ResolvedTool {
    if tool == "$SHELL" || tool == "shell" {
        let fallback = ahand_platform::shell::default_shell();
        ResolvedTool {
            path: shell_env.map(str::to_string).unwrap_or(fallback.path),
            leading_args: fallback.login_args,
        }
    } else {
        ResolvedTool {
            path: tool.to_string(),
            leading_args: Vec::new(),
        }
    }
}
```

At BOTH call sites (line 104 and line 301), replace
`std::env::var("SHELL").ok().as_deref()` with
`ahand_platform::shell::env_shell().as_deref()`.

- [ ] **Step 5: Update `tool_resolution_tests` (executor.rs:540-583)**

The two fallback-sensitive tests become cfg-aware; the rest stay unchanged:

```rust
    #[test]
    fn shell_sentinel_falls_back_to_platform_shell_when_env_is_unset() {
        let r = resolve_tool("$SHELL", None);
        let expected = ahand_platform::shell::default_shell();
        assert_eq!(
            r,
            ResolvedTool {
                path: expected.path,
                leading_args: expected.login_args,
            }
        );
    }
```

(Replaces `shell_sentinel_falls_back_to_bin_sh_when_shell_env_is_unset`. The two explicit-`shell_env` tests keep asserting `-l` ONLY under `#[cfg(unix)]`; add `#[cfg(windows)]` branches asserting empty `leading_args`:)

```rust
    #[test]
    fn dollar_shell_sentinel_resolves_to_shell_env_with_login_flag() {
        let r = resolve_tool("$SHELL", Some("/bin/zsh"));
        #[cfg(unix)]
        let expected_args = vec!["-l".to_string()];
        #[cfg(windows)]
        let expected_args: Vec<String> = vec![];
        assert_eq!(
            r,
            ResolvedTool {
                path: "/bin/zsh".to_string(),
                leading_args: expected_args,
            }
        );
    }
```

(Apply the same pattern to `shell_sentinel_resolves_to_shell_env_with_login_flag`.)

- [ ] **Step 6: Adopt in `openclaw/handler.rs:260-261`**

Replace:

```rust
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(&shell_cmd);
```

with:

```rust
        let shell = ahand_platform::shell::env_shell()
            .unwrap_or_else(|| ahand_platform::shell::default_shell().path);
        let mut cmd = Command::new(shell);
        cmd.arg(ahand_platform::shell::shell_c_flag()).arg(&shell_cmd);
```

- [ ] **Step 7: Verify**

Run: `cargo test -p ahandd && cargo test -p ahand-platform && cargo check --target x86_64-pc-windows-msvc -p ahand-platform && grep -rn '"/bin/sh"' crates/ahandd/src/ | grep -v test`
Expected: tests PASS; grep returns no production-code matches (test fixtures may remain).

- [ ] **Step 8: Commit**

```bash
git add crates/ahand-platform crates/ahandd
git commit -m "feat(platform): cross-platform shell resolution in executor and openclaw"
```

---

### Task 5: Policy verbatim-prefix fix (`\\?\`)

**Goal:** File-policy allow/deny glob matching works on Windows: canonicalized paths are simplified before matching, so patterns like `C:\Users\x\**` match. (Without this, EVERY file op on Windows is PolicyDenied.)

**Files:**
- Modify: `crates/ahandd/Cargo.toml` (add `dunce.workspace = true` — only if policy.rs uses dunce directly; prefer routing through `ahand_platform::paths`)
- Modify: `crates/ahandd/src/file_manager/policy.rs:206-263`
- Test: `crates/ahandd/src/file_manager/policy.rs` (inline `#[cfg(test)]`, follow existing test placement in the file if present, else add module)

**Acceptance Criteria:**
- [ ] `canonicalize_or_parent` / `canonicalize_no_follow` return simplified paths (no `\\?\` prefix) on Windows; unchanged on Unix
- [ ] New regression test proves an allowlisted temp dir passes `check_path` (runs on all platforms; on Windows CI this is THE critical test)
- [ ] Existing `cargo test -p ahandd --test file_ops` still passes on macOS

**Verify:** `cargo test -p ahandd file_manager && cargo test -p ahandd --test file_ops && cargo check --target x86_64-pc-windows-msvc -p ahandd` (msvc check may still fail in later-task files only)

**Steps:**

- [ ] **Step 1: Write the failing regression test** (in `policy.rs`'s test module; create one if absent, mirroring `FilePolicy` construction used by `file_ops.rs::test_manager`)

```rust
    #[test]
    fn allowlisted_canonical_tempdir_passes_check_path() {
        let tmp = tempfile::tempdir().unwrap();
        // Build the pattern the same way operators do: from a plain
        // (non-verbatim) absolute path + /**.
        let root = ahand_platform::paths::canonicalize_simplified(tmp.path()).unwrap();
        let root_str = root.to_string_lossy().into_owned();
        let policy = FilePolicy::new(&crate::config::FilePolicyConfig {
            enabled: true,
            path_allowlist: vec![format!("{}/**", root_str.trim_end_matches('/')), root_str.clone()],
            path_denylist: vec![],
            max_read_bytes: 1_000_000,
            max_write_bytes: 1_000_000,
            dangerous_paths: vec![],
        });
        let file = root.join("hello.txt");
        std::fs::write(&file, b"hi").unwrap();
        let result = policy.check_path(&file.to_string_lossy(), false, false);
        assert!(result.is_ok(), "check_path denied an allowlisted path: {result:?}");
    }
```

(Adapt the constructor name/signature to what `policy.rs` actually exposes — `FileManager::new` in `file_ops.rs` wraps it; the policy-level type is in this file. Use the real one; do not invent.)

NOTE: on macOS this test already passes (no verbatim prefix exists). It is still mandatory: it pins the contract, and on `windows-latest` CI (Task 10) it is the regression test for this fix. Mark with a comment saying so.

- [ ] **Step 2: Apply the fix in `policy.rs`**

In `canonicalize_or_parent` (lines 206-242), route BOTH canonicalize results through simplify:

- line ~208: `Ok(p) => return Ok(p),` → `Ok(p) => return Ok(ahand_platform::paths::simplify(&p)),`
- line ~230-236 (existing-ancestor branch):

```rust
            Ok(canonical) => {
                let mut rebuilt = ahand_platform::paths::simplify(&canonical);
                for part in suffix.iter().rev() {
                    rebuilt.push(part);
                }
                return Ok(rebuilt);
            }
```

`canonicalize_no_follow` needs no change (it builds on `canonicalize_or_parent`).

- [ ] **Step 3: Verify**

Run: `cargo test -p ahandd file_manager && cargo test -p ahandd --test file_ops && cargo test -p ahandd --test file_ops_e2e`
Expected: PASS, including the new regression test.

- [ ] **Step 4: Commit**

```bash
git add crates/ahandd
git commit -m "fix(file-policy): simplify canonicalized paths so allowlists match on Windows"
```

---

### Task 6: `platform::secure_file` + route all three secret writers

**Goal:** One `write_secure_file()` writes owner-only secret files on both platforms (0o600 on Unix; `icacls` inheritance-strip + owner-only grant on Windows); `device_identity.rs` (ahandd), `openclaw/device_identity.rs`, and `openclaw/pairing.rs` all use it.

**Files:**
- Create: `crates/ahand-platform/src/secure_file.rs`
- Modify: `crates/ahand-platform/src/lib.rs`, `crates/ahandd/src/device_identity.rs:192-213`, `crates/ahandd/src/openclaw/device_identity.rs:110-139`, `crates/ahandd/src/openclaw/pairing.rs:70-93`

**Acceptance Criteria:**
- [ ] Unix: file created with mode 0o600 BEFORE contents are written (no chmod-after-write window); asserted by a unit test reading the mode
- [ ] Windows: contents written to a temp file in the same dir, ACL restricted via `icacls /inheritance:r /grant:r <user>:F`, then renamed into place; failure of icacls = hard error (no silently-open secrets)
- [ ] All three call sites delegate; `grep -rn "0o600" crates/ahandd/src/` returns no matches

**Verify:** `cargo test -p ahand-platform secure_file && cargo test -p ahandd && cargo check --target x86_64-pc-windows-msvc -p ahand-platform`

**Steps:**

- [ ] **Step 1: Write failing tests in `src/secure_file.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_contents_and_creates_parents() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("deep").join("nested").join("secret.json");
        write_secure_file(&path, b"{\"k\":1}\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"k\":1}\n");
    }

    #[cfg(unix)]
    #[test]
    fn unix_mode_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("secret");
        write_secure_file(&path, b"s").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(windows)]
    #[test]
    fn windows_acl_is_restricted_to_owner() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("secret");
        write_secure_file(&path, b"s").unwrap();
        // icacls <file> prints one line per ACE; after /inheritance:r and a
        // single grant there must be exactly one ACE, naming the current user.
        let out = std::process::Command::new("icacls").arg(&path).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
        let user = std::env::var("USERNAME").unwrap().to_lowercase();
        assert!(text.contains(&user), "ACL output missing user: {text}");
        let ace_count = text.lines().filter(|l| l.contains(":(")).count()
            + text.lines().filter(|l| l.contains(":f")).count();
        assert!(ace_count >= 1, "no ACE found: {text}");
        assert!(!text.contains("builtin\\users"), "world-readable ACL: {text}");
    }

    #[test]
    fn overwrites_existing_file_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("secret");
        write_secure_file(&path, b"one").unwrap();
        write_secure_file(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ahand-platform secure_file`
Expected: compile FAIL.

- [ ] **Step 3: Implement `src/secure_file.rs`**

```rust
//! Owner-only secret-file writes.
//!
//! Unix: open with mode 0o600 *before* writing (no chmod-after-write window),
//! fsync, atomic rename. Windows: write a temp file, strip ACL inheritance
//! and grant only the current user via `icacls`, then rename into place; the
//! temp file briefly exists with default ACLs inside the target directory,
//! which is itself under the user profile — accepted and documented in the
//! spec ("Behavioral decisions").

use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;

pub fn write_secure_file(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("secure file path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create directory {}", parent.display()))?;

    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
    ));

    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("failed to create {}", tmp.display()))?;
        f.write_all(contents)
            .with_context(|| format!("failed to write {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("failed to fsync {}", tmp.display()))?;
    }

    #[cfg(windows)]
    restrict_to_owner(&tmp).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;

    // Windows: rename-over-existing fails → remove the target first. The
    // window is acceptable for these files (single-writer, same user).
    #[cfg(windows)]
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("failed to replace {}", path.display()))?;
    }

    std::fs::rename(&tmp, path).with_context(|| {
        let _ = std::fs::remove_file(&tmp);
        format!("failed to rename {} -> {}", tmp.display(), path.display())
    })?;
    Ok(())
}

/// Restrict `path` to the current user only (Windows). Hard error on failure:
/// a secret file with default ACLs must never be left in place silently.
#[cfg(windows)]
pub fn restrict_to_owner(path: &Path) -> Result<()> {
    let user = std::env::var("USERNAME").context("USERNAME is not set")?;
    let output = std::process::Command::new("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r", &format!("{user}:F")])
        .output()
        .context("failed to run icacls")?;
    if !output.status.success() {
        anyhow::bail!(
            "icacls failed on {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}
```

Add `pub mod secure_file;` to `lib.rs`. Run: `cargo test -p ahand-platform secure_file` → PASS (Unix tests locally; Windows test compiles, runs in CI).

- [ ] **Step 4: Route the three call sites**

1. `crates/ahandd/src/device_identity.rs:192-213` — replace the body of the existing `write_secure_file` helper with a delegation (keep the local fn so callers/tests don't move):

```rust
fn write_secure_file(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    ahand_platform::secure_file::write_secure_file(path, contents)
}
```

DELETE its old body (OpenOptionsExt/mode/rename logic) and any now-unused imports.

2. `crates/ahandd/src/openclaw/device_identity.rs` (`StoredIdentity::save`, lines ~110-139): replace

```rust
        std::fs::write(path, format!("{}\n", content))
            .with_context(|| format!("failed to write {}", path.display()))?;

        // Set file permissions to 0600 (user read/write only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            let _ = std::fs::set_permissions(path, perms);
        }
```

with:

```rust
        ahand_platform::secure_file::write_secure_file(path, format!("{}\n", content).as_bytes())?;
```

(also remove the now-redundant `create_dir_all` block above it — `write_secure_file` creates parents — and unused imports.)

3. `crates/ahandd/src/openclaw/pairing.rs` (`save_pairing_state`, lines 70-93): same replacement pattern as (2).

- [ ] **Step 5: Verify**

Run: `cargo test -p ahandd && cargo test -p ahand-platform && grep -rn "0o600" crates/ahandd/src/`
Expected: tests PASS; grep no matches. Also run `cargo test -p ahandd --test hello_signature` explicitly (identity write path changed; golden sig must not).

- [ ] **Step 6: Commit**

```bash
git add crates/ahand-platform crates/ahandd
git commit -m "feat(platform): owner-only secure_file writes; route all secret files through it"
```

---

### Task 7: `platform::ipc` module (transport only, with loopback tests)

**Goal:** One IPC transport API — Unix domain socket on Unix, named pipe on Windows — with bind/accept/connect and a loopback round-trip test that runs on both platforms.

**Files:**
- Create: `crates/ahand-platform/src/ipc.rs`
- Modify: `crates/ahand-platform/src/lib.rs`

**Acceptance Criteria:**
- [ ] `IpcEndpoint::default_for_user()`: Unix `~/.ahand/ahandd.sock`; Windows `\\.\pipe\ahandd-<USERNAME>`
- [ ] `IpcListener::bind(&endpoint, mode)` + `accept() -> (IpcServerStream, String peer)`; `ipc_connect(&endpoint) -> IpcClientStream`
- [ ] `IpcServerStream`/`IpcClientStream` are type aliases implementing `AsyncRead + AsyncWrite` (UnixStream / NamedPipeServer / NamedPipeClient)
- [ ] Loopback test: bind, connect, echo bytes both directions — passes on macOS now, windows CI later

**Verify:** `cargo test -p ahand-platform ipc && cargo check --target x86_64-pc-windows-msvc -p ahand-platform`

**Steps:**

- [ ] **Step 1: Write failing loopback test in `src/ipc.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn test_endpoint(tag: &str) -> IpcEndpoint {
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            // Leak: keep dir alive for the test process lifetime (sockets
            // must outlive the tempdir guard inside the async block).
            let path = Box::leak(Box::new(dir)).path().join(format!("{tag}.sock"));
            IpcEndpoint::from_path(path)
        }
        #[cfg(windows)]
        {
            IpcEndpoint::from_path(std::path::PathBuf::from(format!(
                r"\\.\pipe\ahand-test-{tag}-{}",
                std::process::id()
            )))
        }
    }

    #[tokio::test]
    async fn loopback_roundtrip() {
        let ep = test_endpoint("roundtrip");
        let mut listener = IpcListener::bind(&ep, 0o660).expect("bind");
        let server = tokio::spawn(async move {
            let (mut stream, peer) = listener.accept().await.expect("accept");
            assert!(!peer.is_empty());
            let mut buf = [0u8; 4];
            stream.read_exact(&mut buf).await.expect("read");
            assert_eq!(&buf, b"ping");
            stream.write_all(b"pong").await.expect("write");
            stream.flush().await.expect("flush");
        });
        let mut client = ipc_connect(&ep).await.expect("connect");
        client.write_all(b"ping").await.expect("write");
        client.flush().await.expect("flush");
        let mut buf = [0u8; 4];
        client.read_exact(&mut buf).await.expect("read");
        assert_eq!(&buf, b"pong");
        server.await.unwrap();
    }

    #[test]
    fn default_endpoint_shape() {
        let ep = IpcEndpoint::default_for_user();
        let s = ep.as_path().to_string_lossy().into_owned();
        #[cfg(unix)]
        assert!(s.ends_with(".ahand/ahandd.sock"), "{s}");
        #[cfg(windows)]
        assert!(s.starts_with(r"\\.\pipe\ahandd-"), "{s}");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ahand-platform ipc`
Expected: compile FAIL.

- [ ] **Step 3: Implement `src/ipc.rs`**

```rust
//! Cross-platform local IPC transport.
//!
//! Unix: `UnixListener`/`UnixStream` at a filesystem socket path, with a
//! configurable file mode and `peer_cred`-derived peer identity.
//! Windows: named pipes (`\\.\pipe\ahandd-<user>`). The default pipe
//! security descriptor only grants write access to the creating user (plus
//! Administrators/SYSTEM), so cross-user clients cannot connect; the `mode`
//! argument is ignored and peer identity is reported as `"pipe:local"`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct IpcEndpoint(PathBuf);

impl IpcEndpoint {
    pub fn from_path(p: PathBuf) -> Self {
        Self(p)
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Default endpoint for the current user.
    pub fn default_for_user() -> Self {
        #[cfg(unix)]
        {
            let base = dirs_home().join(".ahand").join("ahandd.sock");
            Self(base)
        }
        #[cfg(windows)]
        {
            let user = std::env::var("USERNAME").unwrap_or_else(|_| "default".into());
            Self(PathBuf::from(format!(r"\\.\pipe\ahandd-{user}")))
        }
    }
}

#[cfg(unix)]
fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(unix)]
pub type IpcServerStream = tokio::net::UnixStream;
#[cfg(unix)]
pub type IpcClientStream = tokio::net::UnixStream;
#[cfg(windows)]
pub type IpcServerStream = tokio::net::windows::named_pipe::NamedPipeServer;
#[cfg(windows)]
pub type IpcClientStream = tokio::net::windows::named_pipe::NamedPipeClient;

pub struct IpcListener {
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
    #[cfg(windows)]
    next: Option<tokio::net::windows::named_pipe::NamedPipeServer>,
    #[cfg(windows)]
    name: String,
}

impl IpcListener {
    /// Bind the endpoint. `mode` is the Unix socket file mode (e.g. 0o660);
    /// ignored on Windows (see module docs).
    #[allow(unused_variables)]
    pub fn bind(endpoint: &IpcEndpoint, mode: u32) -> Result<Self> {
        #[cfg(unix)]
        {
            let path = endpoint.as_path();
            // Remove stale socket file if it exists.
            let _ = std::fs::remove_file(path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let inner = tokio::net::UnixListener::bind(path)
                .with_context(|| format!("bind {}", path.display()))?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .with_context(|| format!("chmod {:04o} {}", mode, path.display()))?;
            Ok(Self { inner })
        }
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ServerOptions;
            let name = endpoint.as_path().to_string_lossy().into_owned();
            let first = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&name)
                .with_context(|| format!("create named pipe {name}"))?;
            Ok(Self { next: Some(first), name })
        }
    }

    /// Accept one connection; returns the stream and a peer-identity string
    /// (`"uid:<n>"` on Unix, `"pipe:local"` on Windows).
    pub async fn accept(&mut self) -> Result<(IpcServerStream, String)> {
        #[cfg(unix)]
        {
            let (stream, _addr) = self.inner.accept().await.context("IPC accept")?;
            let peer = match stream.peer_cred() {
                Ok(cred) => format!("uid:{}", cred.uid()),
                Err(_) => "uid:unknown".to_string(),
            };
            Ok((stream, peer))
        }
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ServerOptions;
            let server = self
                .next
                .take()
                .expect("IpcListener::accept called after a failed accept");
            server.connect().await.context("IPC pipe connect")?;
            // Pre-create the next instance so no client hits "no instance".
            self.next = Some(
                ServerOptions::new()
                    .create(&self.name)
                    .with_context(|| format!("recreate named pipe {}", self.name))?,
            );
            Ok((server, "pipe:local".to_string()))
        }
    }
}

/// Connect to the daemon IPC endpoint.
pub async fn ipc_connect(endpoint: &IpcEndpoint) -> Result<IpcClientStream> {
    #[cfg(unix)]
    {
        tokio::net::UnixStream::connect(endpoint.as_path())
            .await
            .with_context(|| format!("connect {}", endpoint.as_path().display()))
    }
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ClientOptions;
        const ERROR_PIPE_BUSY: i32 = 231;
        let name = endpoint.as_path().to_string_lossy().into_owned();
        loop {
            match ClientOptions::new().open(&name) {
                Ok(client) => return Ok(client),
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(e) => return Err(e).with_context(|| format!("connect {name}")),
            }
        }
    }
}
```

Add `pub mod ipc;` to `lib.rs`.

NOTE (Unix `dirs_home`): `ahand-platform` deliberately avoids the `dirs` crate to keep deps minimal; `HOME` is always set in practice (config loading already hard-fails when it is not — see `expand_tilde_with` I2 note in `crates/ahandd/src/config.rs:278`).

- [ ] **Step 4: Verify**

Run: `cargo test -p ahand-platform ipc && cargo check --target x86_64-pc-windows-msvc -p ahand-platform && cargo clippy -p ahand-platform --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ahand-platform
git commit -m "feat(platform): cross-platform IPC transport (unix socket / named pipe)"
```

---

### Task 8: Adopt `platform::ipc` in `ahandd` server + `ahandctl` client

**Goal:** `serve_ipc` and all five `ahandctl` IPC entry points run on the platform transport; `ipc.rs` compiles for msvc; `--ipc`/config semantics documented for pipe names.

**Files:**
- Modify: `crates/ahandd/src/ipc.rs:1-9,18-79,479-483`, `crates/ahandd/src/config.rs:441-455`, `crates/ahandd/src/main.rs` (serve_ipc call site)
- Modify: `crates/ahandctl/src/main.rs:282-305` (read/write_frame stay; connect sites change)

**Acceptance Criteria:**
- [ ] `ahandd/src/ipc.rs` has no `UnixListener`/`UnixStream`/`PermissionsExt` references; `handle_ipc_conn` is generic over `AsyncRead + AsyncWrite + Unpin + Send + 'static` (or takes the alias type)
- [ ] `config.ipc_socket_path()` returns `IpcEndpoint`: configured value if set, else `IpcEndpoint::default_for_user()`; no `/tmp` fallback
- [ ] `ahandctl --ipc <path>` connects via `ipc_connect` (a pipe name on Windows)
- [ ] `cargo check --target x86_64-pc-windows-msvc -p ahandd -p ahandctl` passes EXCEPT errors in `updater.rs`/`browser_setup`/`openclaw` files owned by Task 9 / M3 (record any)

**Verify:** `cargo test -p ahandd -p ahandctl && cargo check --target x86_64-pc-windows-msvc -p ahandd -p ahandctl`

**Steps:**

- [ ] **Step 1: Refactor `ahandd/src/ipc.rs`**

1. Imports (lines 1-9): remove `tokio::net::{UnixListener, UnixStream}`; add `use ahand_platform::ipc::{IpcEndpoint, IpcListener};`.
2. `serve_ipc` signature: `socket_path: PathBuf` → `endpoint: IpcEndpoint` (callers pass `cfg.ipc_socket_path()`); body becomes:

```rust
    let mut listener = IpcListener::bind(&endpoint, socket_mode)?;
    info!(endpoint = %endpoint.as_path().display(), "IPC server listening");

    loop {
        match listener.accept().await {
            Ok((stream, caller_id)) => {
                let reg = Arc::clone(&registry);
                let st = store.clone();
                let smgr = Arc::clone(&session_mgr);
                let amgr = Arc::clone(&approval_mgr);
                let bcast = approval_broadcast_tx.clone();
                let did = device_id.clone();
                let bmgr = Arc::clone(&browser_mgr);
                tokio::spawn(async move {
                    if let Err(e) =
                        handle_ipc_conn(stream, reg, st, smgr, amgr, bcast, did, caller_id, bmgr)
                            .await
                    {
                        warn!(error = %e, "IPC connection error");
                    }
                });
            }
            Err(e) => {
                error!(error = %e, "IPC accept error");
            }
        }
    }
```

(The stale-socket removal, parent-dir creation, permission setting, and peer_cred reading all moved INTO `IpcListener`.)

3. `handle_ipc_conn`: change its first parameter from `UnixStream` to generic:

```rust
async fn handle_ipc_conn<S>(
    stream: S,
    /* ...other params unchanged... */
) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
```

Inside, replace `stream.into_split()` with `tokio::io::split(stream)` (works for any AsyncRead+AsyncWrite; adjust the reader/writer variable types accordingly — they become `ReadHalf<S>`/`WriteHalf<S>`).

4. DELETE `fn set_permissions` (lines 479-483) — now inside the platform crate.

- [ ] **Step 2: Update `config.rs:441-455`**

```rust
    /// Resolve the IPC endpoint. Default: per-user platform endpoint
    /// (`~/.ahand/ahandd.sock` on Unix, `\\.\pipe\ahandd-<user>` on Windows).
    pub fn ipc_socket_path(&self) -> ahand_platform::ipc::IpcEndpoint {
        match &self.ipc_socket_path {
            Some(p) => ahand_platform::ipc::IpcEndpoint::from_path(PathBuf::from(p)),
            None => ahand_platform::ipc::IpcEndpoint::default_for_user(),
        }
    }
```

Fix the `serve_ipc` call site in `main.rs` to pass this through (types now line up; no `/tmp` fallback remains). `ipc_socket_mode()` stays as-is (ignored on Windows by `bind`).

- [ ] **Step 3: Update `ahandctl/src/main.rs` connect sites**

The five `ipc_*` functions each do `tokio::net::UnixStream::connect(socket_path)`. Replace each with:

```rust
    let endpoint = ahand_platform::ipc::IpcEndpoint::from_path(std::path::PathBuf::from(ipc_path));
    let stream = ahand_platform::ipc::ipc_connect(&endpoint).await?;
```

then `tokio::io::split(stream)` where the code previously split the UnixStream. `read_frame`/`write_frame` (lines 282-300) are already generic over `AsyncReadExt/AsyncWriteExt` — unchanged. Update the `--ipc` arg help text to: `"IPC endpoint (Unix socket path; named pipe name on Windows, e.g. \\\\.\\pipe\\ahandd-<user>)"`.

- [ ] **Step 4: Verify**

Run: `cargo test -p ahandd -p ahandctl && cargo check --target x86_64-pc-windows-msvc -p ahandd -p ahandctl 2>&1 | tee /tmp/msvc-check.txt`
Expected: tests PASS on macOS. The msvc check: zero errors in `ipc.rs`/`config.rs`/`main.rs`; any remaining errors must be in `updater.rs` (Task 9) or `browser_setup`/`openclaw`/`file_manager::fs_ops` symlink-mode code (M3/M4 scope — if fs_ops fails the msvc build, gate ONLY the offending fn with `#[cfg(unix)]` + a Windows arm returning `FileErrorCode::PolicyDenied`-style unsupported error, and note it for M4).

- [ ] **Step 5: Manual smoke (macOS)**

```bash
cargo build -p ahandd -p ahandctl
AHAND_DEBUG_IPC=1 AHAND_IPC_SOCKET=/tmp/ahand-smoke.sock target/debug/ahandd --mode ahand-cloud --url ws://127.0.0.1:1/ws &
sleep 1
target/debug/ahandctl --ipc /tmp/ahand-smoke.sock session status; echo "exit=$?"
kill %1
```

Expected: ahandctl talks to the daemon over IPC (any well-formed response, even an error envelope, proves transport works; `exit=0` for session status).

- [ ] **Step 6: Commit**

```bash
git add crates/ahandd crates/ahandctl
git commit -m "feat(ipc): run daemon IPC on platform transport (named pipes on Windows)"
```

---

### Task 9: Updater + remaining `/tmp` fallbacks + `save_atomic`

**Goal:** Self-update works on Windows (`.exe`, rename-aside, spawn-restart); no `/tmp` literals remain in client-crate production code; `save_atomic` documented+tested for Windows replace semantics.

**Files:**
- Modify: `crates/ahandd/src/updater.rs:261-351`, `crates/ahandd/src/main.rs` (startup cleanup hook), `crates/ahandd/src/browser.rs:55-65,416-434`, `crates/ahandd/src/config.rs:403-435`
- Test: `crates/ahandd/src/updater.rs` inline tests; `crates/ahandd/src/config.rs` inline test

**Acceptance Criteria:**
- [ ] `install_binary` writes `ahandd.exe` on Windows, renames the running binary aside to `ahandd.exe.old` before the swap, and a startup hook removes stale `.old`
- [ ] `restart_daemon`: Unix `exec()` unchanged; Windows spawns the new binary detached with same args and exits 0
- [ ] `browser.rs` `/tmp` fallbacks → `std::env::temp_dir()`; no `"/tmp"` literals in `crates/ahandd/src` or `crates/ahandctl/src` outside tests
- [ ] `save_atomic` has a test overwriting an existing config; comment updated (no "Unix-only for now" claim)

**Verify:** `cargo test -p ahandd updater && cargo test -p ahandd config && cargo check --target x86_64-pc-windows-msvc -p ahandd && ! grep -rn '"/tmp"' crates/ahandd/src crates/ahandctl/src --include='*.rs' | grep -v test`

**Steps:**

- [ ] **Step 1: Write failing tests (updater.rs inline `#[cfg(test)]`)**

```rust
#[cfg(test)]
mod install_tests {
    use super::*;

    #[test]
    fn install_binary_into_writes_exe_named_binary_and_version() {
        let tmp = tempfile::tempdir().unwrap();
        install_binary_into(tmp.path(), b"fake-binary", "9.9.9").unwrap();
        let bin = tmp
            .path()
            .join("bin")
            .join(ahand_platform::paths::exe_name("ahandd"));
        assert_eq!(std::fs::read(&bin).unwrap(), b"fake-binary");
        assert_eq!(std::fs::read_to_string(tmp.path().join("version")).unwrap(), "9.9.9");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&bin).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "binary not executable");
        }
    }

    #[test]
    fn install_binary_into_replaces_existing_and_leaves_old_aside_semantics() {
        let tmp = tempfile::tempdir().unwrap();
        install_binary_into(tmp.path(), b"v1", "1").unwrap();
        install_binary_into(tmp.path(), b"v2", "2").unwrap();
        let bin = tmp
            .path()
            .join("bin")
            .join(ahand_platform::paths::exe_name("ahandd"));
        assert_eq!(std::fs::read(&bin).unwrap(), b"v2");
        // .old must not accumulate on Unix; on Windows it may exist after a
        // live swap and is cleaned by cleanup_old_binary_in().
        cleanup_old_binary_in(tmp.path());
        assert!(!tmp.path().join("bin").join("ahandd.exe.old").exists());
    }
}
```

- [ ] **Step 2: Refactor `install_binary` → testable `install_binary_into(ahand_home, data, version)`**

```rust
fn install_binary(data: &[u8], target_version: &str) -> anyhow::Result<()> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    install_binary_into(&home.join(".ahand"), data, target_version)
}

fn install_binary_into(ahand_home: &Path, data: &[u8], target_version: &str) -> anyhow::Result<()> {
    let bin_dir = ahand_home.join("bin");
    std::fs::create_dir_all(&bin_dir)?;

    let bin_name = ahand_platform::paths::exe_name("ahandd");
    let target_path = bin_dir.join(&bin_name);
    let tmp_path = bin_dir.join(format!("{bin_name}.update.tmp"));

    // Write to temp file, then swap.
    std::fs::write(&tmp_path, data)?;

    // Make executable (Unix).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&tmp_path, perms)?;
    }

    // Windows cannot rename over a running executable's image, but CAN
    // rename the running image aside. Standard self-update pattern:
    // running.exe -> running.exe.old, new -> running.exe, clean .old later.
    #[cfg(windows)]
    if target_path.exists() {
        let old_path = bin_dir.join(format!("{bin_name}.old"));
        let _ = std::fs::remove_file(&old_path);
        std::fs::rename(&target_path, &old_path)?;
    }

    std::fs::rename(&tmp_path, &target_path)?;
    info!(path = %target_path.display(), "installed new binary");

    // Write version marker.
    let version_path = ahand_home.join("version");
    std::fs::write(&version_path, target_version)?;
    info!(version = %target_version, "wrote version marker");

    Ok(())
}

/// Remove a stale `.old` binary left by a previous Windows self-update.
/// Call at daemon startup; harmless no-op everywhere else.
pub fn cleanup_old_binary() {
    if let Some(home) = dirs::home_dir() {
        cleanup_old_binary_in(&home.join(".ahand"));
    }
}

fn cleanup_old_binary_in(ahand_home: &Path) {
    let old = ahand_home
        .join("bin")
        .join(format!("{}.old", ahand_platform::paths::exe_name("ahandd")));
    if old.exists() {
        if let Err(e) = std::fs::remove_file(&old) {
            tracing::warn!(error = %e, path = %old.display(), "failed to remove stale .old binary");
        }
    }
}
```

(`use std::path::Path;` if not already imported.)

- [ ] **Step 3: Windows `restart_daemon`**

Replace the `#[cfg(not(unix))]` bail arm (updater.rs:347-350) with:

```rust
    #[cfg(windows)]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut cmd = std::process::Command::new(&bin_path);
        cmd.args(&args);
        ahand_platform::process::configure_detached(&mut cmd);
        cmd.spawn()
            .map_err(|e| anyhow::anyhow!("failed to spawn new daemon: {e}"))?;
        info!("spawned new daemon binary; exiting current process");
        std::process::exit(0);
    }
```

and change `let bin_path = home.join(".ahand").join("bin").join("ahandd");` to use `ahand_platform::paths::exe_name("ahandd")`.

- [ ] **Step 4: Startup cleanup hook**

In `ahandd/src/main.rs`, immediately after the PID file write block (line ~271-275), add:

```rust
    // Clean up any stale binary left by a previous Windows self-update.
    updater::cleanup_old_binary();
```

- [ ] **Step 5: `/tmp` fallbacks**

- `browser.rs:58` and `browser.rs:419`: `PathBuf::from("/tmp")` → `std::env::temp_dir()`.
- Confirm `config.rs` `/tmp` fallback already removed by Task 8 Step 2.

- [ ] **Step 6: `save_atomic` Windows semantics**

`std::fs::rename` on Windows uses `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` and replaces existing files. Update the stale comment (config.rs:425-427) to:

```rust
        // Rename (atomic on POSIX; on Windows std::fs::rename replaces an
        // existing target via MoveFileExW(MOVEFILE_REPLACE_EXISTING), which
        // can still fail if another process holds the file open — the
        // overwrite test below pins the replace behavior on all platforms).
```

Add an inline test next to existing config tests:

```rust
    #[test]
    fn save_atomic_replaces_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        let cfg = Config::default();
        cfg.save_atomic(&path).unwrap();
        cfg.save_atomic(&path).unwrap(); // second save must not error
        assert!(path.exists());
        assert!(!path.with_extension("toml.tmp").exists());
    }
```

(If `Config` has no `Default`, construct the minimal config the existing tests in the file use — follow the file's own fixtures. If `save_atomic` is private, the test lives in the same module — it already is in `config.rs`.)

- [ ] **Step 7: Verify**

Run the full Verify line from the task header.
Expected: all PASS, grep finds nothing.

- [ ] **Step 8: Commit**

```bash
git add crates/ahandd
git commit -m "feat(updater): Windows-safe self-update (.exe, rename-aside, spawn-restart); drop /tmp fallbacks"
```

---

### Task 10: CI — 3-OS client test lane + Windows release artifacts

**Goal:** Every PR runs client-crate tests on ubuntu/macos/windows; release publishes `*-windows-x64.exe` artifacts.

**Files:**
- Create: `.github/workflows/client-ci.yml`
- Modify: `.github/workflows/release-rust.yml:19-56,76-83`

**Acceptance Criteria:**
- [ ] client-ci runs `fmt` (ubuntu only), `clippy -D warnings`, and `cargo test -p ahand-platform -p ahandd -p ahandctl --all-targets` on all 3 OSes
- [ ] Windows job green — including the policy regression test (Task 5) and platform loopback/secure-file tests
- [ ] release-rust.yml matrix includes `x86_64-pc-windows-msvc` with protoc, `.exe` artifact naming `ahandd-windows-x64.exe`
- [ ] Pre-existing `#[cfg(unix)]`-gated tests simply don't run on Windows (expected); any test that FAILS on Windows gets fixed if trivial (path strings, tempdir) or `#[cfg(unix)]`-gated with a `// TODO(M4): port to Windows` note — recorded in the task report

**Verify:** push branch → `gh run watch` both workflows green; `gh run list --workflow=client-ci.yml --limit 1`

**Steps:**

- [ ] **Step 1: Create `.github/workflows/client-ci.yml`**

```yaml
name: Client CI

on:
  pull_request:
    paths:
      - "crates/**"
      - "proto/**"
      - "Cargo.toml"
      - "Cargo.lock"
      - ".github/workflows/client-ci.yml"
  push:
    branches: [main]

jobs:
  test:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v6

      - name: Install system dependencies (Linux)
        if: runner.os == 'Linux'
        run: sudo apt-get update && sudo apt-get install -y protobuf-compiler libssl-dev pkg-config

      - name: Install protoc (macOS)
        if: runner.os == 'macOS'
        run: brew install protobuf

      - name: Install protoc (Windows)
        if: runner.os == 'Windows'
        uses: arduino/setup-protoc@v3
        with:
          repo-token: ${{ secrets.GITHUB_TOKEN }}

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy

      - uses: Swatinem/rust-cache@v2

      - name: Check formatting
        if: runner.os == 'Linux'
        run: cargo fmt -p ahand-platform -p ahandd -p ahandctl --check

      - name: Clippy
        run: cargo clippy -p ahand-platform -p ahandd -p ahandctl --all-targets -- -D warnings

      - name: Test
        run: cargo test -p ahand-platform -p ahandd -p ahandctl --all-targets
```

- [ ] **Step 2: Enable Windows in `release-rust.yml`**

Replace the commented block (lines 34-37) with a real matrix entry:

```yaml
          - os: windows-latest
            target: x86_64-pc-windows-msvc
            suffix: windows-x64
```

Uncomment/restore the protoc step (lines 52-56):

```yaml
      - name: Install protoc (Windows)
        if: runner.os == 'Windows'
        uses: arduino/setup-protoc@v3
        with:
          repo-token: ${{ secrets.GITHUB_TOKEN }}
```

Replace the `Prepare artifacts` step (lines 76-83) with an `.exe`-aware version:

```yaml
      - name: Prepare artifacts
        shell: bash
        run: |
          mkdir -p release
          EXT=""
          if [ "${{ runner.os }}" = "Windows" ]; then EXT=".exe"; fi
          cp "target/${{ matrix.target }}/release/ahandd$EXT" "release/ahandd-${{ matrix.suffix }}$EXT"
          cp "target/${{ matrix.target }}/release/ahandctl$EXT" "release/ahandctl-${{ matrix.suffix }}$EXT"
          cd release
          if command -v sha256sum >/dev/null 2>&1; then
            sha256sum * > "checksums-rust-${{ matrix.suffix }}.txt"
          else
            shasum -a 256 * > "checksums-rust-${{ matrix.suffix }}.txt"
          fi
```

(Leave the `universal` job untouched — it only consumes darwin artifacts. If a later release job globs ALL artifacts, the `.exe` files flow through by name.)

- [ ] **Step 3: Push and watch**

```bash
git add .github/workflows/client-ci.yml .github/workflows/release-rust.yml
git commit -m "ci: 3-OS client test lane; enable windows-x64 release artifacts"
git push -u origin feat/cross-platform-support
gh run list --branch feat/cross-platform-support
gh run watch <client-ci-run-id> --exit-status
```

Expected: client-ci green on all 3 OSes. THIS IS THE M1 GATE. If the Windows job fails:
- test failures from Unix path assumptions in tests → fix the test (tempdir/`os.tmpdir` style) when trivial, else gate `#[cfg(unix)]` + `// TODO(M4): port to Windows` and record in the report;
- compile failures in `browser_setup`/`openclaw` PATH code → these belong to M3/M4; gate the *minimal* offending fn with `#[cfg]` arms returning a clear unsupported error, record it, and keep the build green;
- iterate until green. Do NOT mark this task complete with a red Windows lane.

- [ ] **Step 4: Release dry-run (workflow_dispatch)**

```bash
gh workflow run release-rust.yml -f tag=rust-v0.0.0-m1-dryrun
gh run watch --exit-status
```

Expected: windows-x64 job produces `ahandd-windows-x64.exe`, `ahandctl-windows-x64.exe`, checksums. (If the workflow's later steps require a real tag/release context and fail at upload, the BUILD job being green is sufficient for M1; record the upload behavior in the report. Delete any draft artifacts created by the dry-run.)

- [ ] **Step 5: Commit any fixups & final verify**

```bash
cargo test --workspace && cargo check --workspace
git status --short   # must be clean
```

---

## Self-Review (performed at plan-writing time)

- **Spec coverage (M1 items):** platform crate ✅(T1) ipc ✅(T7,T8) process ✅(T2) shell ✅(T4) paths/policy `\\?\` ✅(T1,T5) secure_file ✅(T6) signals ✅(T3) restart/rename-aside ✅(T9) `.exe` ✅(T2,T9,T10) `/tmp`→temp_dir ✅(T8,T9) `.gitattributes` ✅(T1, pulled forward from M4 because Windows CI in T10 needs LF-stable checkouts) Windows CI compile+test ✅(T10). Out of M1 (per spec): browser PATH `:` fixes (M3), sanitize_env (M4), BATS-on-ubuntu (M5), install.ps1 (M2).
- **Known deviation:** `peer_cred`-equivalent identity on Windows is reported as `"pipe:local"` relying on default pipe SD restricting cross-user write access — matches spec's "default per-user pipe ACL replaces peer_cred".
- **Type consistency:** `IpcEndpoint`/`IpcListener`/`ipc_connect` names consistent across T7/T8; `TerminateMode` across T2; `exe_name` across T2/T9; `install_binary_into`/`cleanup_old_binary` self-consistent in T9.
- **Placeholder scan:** no TBDs; the two "record and move on" branches (T3/T8 msvc partial-check, T10 test gating) are explicit, bounded fallback procedures, not deferrals.
