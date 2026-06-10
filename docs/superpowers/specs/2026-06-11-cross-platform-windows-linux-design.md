# Cross-Platform Support Design: Native Windows + Linux

**Date:** 2026-06-11
**Status:** Approved in design review with user
**Scope:** All end-user features of aHand on native Windows and Linux

## Goal

Every end-user feature of aHand — daemon (`ahandd`), CLI (`ahandctl`), shell job
execution, file operations, browser automation, admin panel, install/upgrade —
must work on native Windows and Linux, with the same one-line install
experience macOS has today.

Decisions fixed during requirements review:

- **Native Windows** (not WSL): `ahandd.exe` / `ahandctl.exe` run directly on Windows.
- **Windows first**; Linux validation runs alongside (Linux is already mostly working).
- **Install via PowerShell one-liner** (`irm https://.../install.ps1 | iex`),
  mirroring the existing `curl | bash` flow. winget/scoop/MSI are out of scope.
- Minimum supported Windows: **Windows 10 1809+** (conpty requirement for PTY).

## Current State (audit summary, 2026-06-11)

A 7-area multi-agent audit (121 findings + 11 critic additions) concluded:

- **Linux: mostly works already.** `install.sh` supports linux; release CI
  publishes `x86_64`/`aarch64-unknown-linux-gnu`. Remaining: e2e never runs on
  Linux CI, a few macOS-flavored user messages, optional musl target.
- **Windows: broken.** ~50 blocker-level findings in 8 clusters:

| # | Cluster | Representative findings |
|---|---------|------------------------|
| 1 | IPC | `crates/ahandd/src/ipc.rs` is `UnixListener`/`UnixStream` only; `config.rs` defaults the socket to `/tmp`. Windows release build is commented out in CI because of this. |
| 2 | Process/signals | `ahandctl/src/daemon.rs:92-137` shells out to `kill`, uses `process_group(0)`; `ahandd/src/main.rs:26` uses `tokio::signal::unix`; `updater.rs:330` restarts via Unix `exec()`. |
| 3 | Shell execution | `ahandd/src/executor.rs:65-77` hardcodes `/bin/sh` and reads `SHELL`. |
| 4 | Install/upgrade chain | `install.sh`/`upgrade.sh`/`setup-browser.sh` are bash-only with hardcoded `/tmp`; `ahandctl` (`upgrade.rs:16`, `browser_init.rs:19`, `admin.rs:381`) spawns bash to run them; no `.exe` suffix handling anywhere. |
| 5 | File-ops security model | `file_manager/io_safe.rs` (openat2 TOCTOU protection) is `#[cfg(unix)]`; **`file_manager/policy.rs:153` glob-matches canonicalized paths, which on Windows carry the `\\?\` verbatim prefix — every file op would be PolicyDenied even after IPC works**; chmod/0o600 paths are Unix-only. |
| 6 | Browser automation | `browser_setup/node.rs` only downloads `.tar.xz` and expects `node/bin/node` (Windows ships `node.exe` at archive root, zip only); `playwright.rs` spawns `npm`/`playwright-cli` directly (`.cmd` shims cannot be spawned by CreateProcess); `browser.rs:465` joins PATH with `':'`. |
| 7 | Self-update | `updater.rs:319` renames over the running binary — impossible on Windows (image file locked); needs rename-aside. |
| 8 | CI/tests | No windows runner in any workflow; release matrix has Windows commented out (incl. protoc setup); artifacts lack `.exe` naming; all file-ops security regression tests are `#[cfg(unix)]` — a Windows port would land with zero security test coverage. |

Notable degraded-level findings: three secret files (`device_identity.rs` ×2 in
ahandd and openclaw, openclaw `pairing.rs`) rely on `0o600` with no Windows
equivalent; `openclaw/handler.rs` `sanitize_env` blocks only `DYLD_/LD_`
loader-injection vars and validates PATH overrides with `':'`-based string
checks; `config.rs` `save_atomic()` rename is non-atomic on Windows; the repo
has no `.gitattributes` (CRLF checkout breaks scripts and golden-string tests);
SDK tests hardcode `/tmp` paths; `node.rs:129` suggests `brew install node` to
all platforms.

Server-side components (hub, hub-dashboard, deploy scripts) are Linux
containers by design and are **out of scope**.

## Architecture

### Platform abstraction layer

A new `platform` layer owns all OS differences. Code shared by `ahandd` and
`ahandctl` (process management, secure files, paths) lives in a new workspace
crate `ahand-platform`; daemon-only pieces (IPC server) stay in
`crates/ahandd/src/platform/`. The rest of the codebase calls these APIs and
contains no `#[cfg]` except trivial one-liners (e.g. `.exe` suffix constants
re-exported from the platform layer).

| Module | Unix implementation (current behavior) | Windows implementation |
|--------|----------------------------------------|------------------------|
| `ipc` | `UnixListener`/`UnixStream`, socket at `~/.ahand/ahandd.sock`, `peer_cred` check, `0o660` | `tokio::net::windows::named_pipe`, pipe name `\\.\pipe\ahandd-<username>`, default per-user pipe ACL replaces `peer_cred` |
| `process` | spawn detached via `process_group(0)`; terminate via SIGTERM→SIGKILL; liveness via `/proc` / `ps` | spawn with `DETACHED_PROCESS \| CREATE_NO_WINDOW`; terminate via `taskkill` / `TerminateProcess`; liveness via Win32 process query |
| `shell` | `SHELL` env → fallback `/bin/sh`; `sh -c <cmd>` | `COMSPEC` env → fallback `cmd.exe`; `cmd /C <cmd>`; config may override to PowerShell |
| `secure_file` | open with mode `0o600` before write | write then restrict ACL to owner; single `write_secure_file()` used by all three secret-file writers |
| `paths` | tilde expansion via `dirs::home_dir()` (unchanged) | same, plus: strip `\\?\`/`\\?\UNC\` after `canonicalize` (via `dunce`) **before policy glob matching**; `std::env::join_paths` for all PATH construction; `std::env::temp_dir()` replaces `/tmp`; `.exe` suffix helper |
| `restart` | `exec()` self-replace | spawn new process, exit current; self-update uses rename-aside (rename running `ahandd.exe` → `.old`, move new into place, clean `.old` on next start) |
| `signals` | `tokio::signal::unix` SIGTERM/SIGINT | `tokio::signal::ctrl_c()`; unified in one `shutdown_signal()` future |

### Install / upgrade / browser-setup chain

Shell-script logic moves into Rust so one binary behaves identically on all
three platforms; bootstrap scripts stay thin.

- `upgrade.sh` logic is rewritten into `ahandctl upgrade` (download, checksum
  verify, stop daemon, swap binaries with `.exe` awareness and rename-aside,
  restart). `ahandctl/src/upgrade.rs` no longer spawns bash.
- `setup-browser.sh` logic is absorbed by the existing `browser_setup` modules;
  `ahandctl` (`browser_init.rs`) and the admin panel handler (`admin.rs:381`)
  call the Rust modules directly instead of spawning bash.
- `browser_setup/node.rs` becomes layout-aware: Unix downloads `.tar.xz` with
  `node/bin/node`; Windows downloads `.zip` with `node.exe` at archive root
  (normalized into a consistent layout at install time). npm and playwright-cli
  are invoked on Windows via `node.exe <js-entrypoint>` (or `cmd /C *.cmd`),
  never by spawning the `.cmd` shim directly.
- Bootstrap installers: existing `scripts/dist/install.sh` (macOS/Linux) plus
  new `scripts/dist/install.ps1` (`irm <url> | iex`): download + extract the
  Windows zip, place binaries under `%USERPROFILE%\.ahand\bin`, print PATH
  setup instructions (`$env:Path`, persistent via user environment).
- Config/data stay under `~/.ahand` (`%USERPROFILE%\.ahand` on Windows) —
  matches existing `dirs::home_dir()` resolution; no migration needed.

### Behavioral decisions

- **FileChmod (Unix mode) returns an explicit unsupported error on Windows**
  (no silent ignore). `FileMkdir.mode` is documented as Unix-only. A full
  Windows ACL mapping (proto already has ACL fields) is a separate future project.
- **TOCTOU protection on Windows is approximated by handle verification**:
  after opening, `GetFinalPathNameByHandle` re-checks the real path against
  policy. Weaker than openat2; documented as such.
- **The daemon runs as a detached process on Windows** — no Windows service
  registration in v1 (parity with macOS, which has no launchd integration
  either). Auto-start is future work.
- **`sanitize_env` gains a Windows blocklist** for loader/lookup injection
  vectors (e.g. `PATHEXT` tampering) and compares PATH overrides via
  `env::split_paths` instead of `':'` string checks.
- Symlink file ops on Windows use `std::os::windows::fs::symlink_file/dir`;
  where privilege is missing (non-developer-mode), return a clear error rather
  than silently skipping.
- **Policy patterns must be written as plain paths** (`C:\Users\...`, never a
  `\\?\` verbatim prefix). Paths that dunce cannot simplify — verbatim-UNC
  (`\\?\UNC\...`) network shares and over-MAX_PATH paths — keep their prefix
  and therefore fail allowlist matching: such ops **deny by default** in M1
  (fail-closed; recorded decision, not a bug). Windows glob matching is
  case-sensitive while the filesystem is not — case-mismatch hardening and
  tests land in M4.

## CI / Release

- Release matrix adds `x86_64-pc-windows-msvc` on `windows-latest`
  (un-comment and finish the existing TODO block, including the protoc setup
  step). Artifact: `ahandd-windows-x64.exe` etc., packaged as zip.
- PR CI adds `cargo test --workspace` on `windows-latest` and `ubuntu-latest`
  in addition to macOS.
- `test-dist-scripts.yml` gains a `windows-latest` job exercising
  `install.ps1` end-to-end against mocked releases.
- New `.gitattributes`: `* text=auto`, `*.sh text eol=lf`, `*.bats text eol=lf`,
  plus `eol=lf` for byte-compared fixtures.

## Testing

- Every `platform` module has unit tests on each OS (run by the new CI matrix);
  100% coverage on new code per project policy.
- File-ops security regression tests (policy allowlist matching, path escape,
  symlink/junction escape, secure-file permissions) gain Windows variants:
  junction-based escapes, ACL assertions instead of mode bits, `\\?\`-prefix
  policy matching tests with an allowlisted temp dir.
- E2E: the existing BATS suite starts running on `ubuntu-latest` (validates
  Linux); Windows gets Rust integration tests for the daemon/CLI lifecycle plus
  a PowerShell install-smoke job. BATS is not ported to Windows.
- SDK tests replace hardcoded `/tmp` paths with `path.join(os.tmpdir(), ...)`.

## Milestones (Windows first)

1. **M1 — Windows core runtime.** `ahand-platform` crate + `platform` modules
   (ipc/process/shell/paths/secure_file/signals/restart), policy `\\?\` prefix
   fix, `.exe` handling, `/bin/sh`→`COMSPEC`, `/tmp`→`temp_dir()`. Windows CI
   compiles and tests green. *Exit: daemon connects to hub from Windows and
   executes shell + file jobs.*
2. **M2 — Lifecycle & install.** `ahandctl start/stop/status/restart` native on
   Windows, upgrade rewritten in Rust with rename-aside, `install.ps1`,
   release matrix ships Windows artifacts. *Exit: PowerShell one-liner installs
   a working, upgradeable daemon.*
3. **M3 — Browser automation.** Node zip layout, npm/playwright-cli invocation,
   browser detection verified on Windows, PATH joining fixed. *Exit: browser
   jobs pass on a Windows runner.*
4. **M4 — Security & test completion.** Owner-only ACL for the three secret
   files, `sanitize_env` Windows blocklist, Windows security regression tests,
   `.gitattributes`. *Exit: security test suite runs on Windows CI with
   coverage parity.*
5. **M5 — Linux validation.** BATS e2e on `ubuntu-latest`, platform-conditional
   user messages (drop `brew` hint), per-platform install docs, admin-panel
   placeholder paths resolved from `/api/status`. *Exit: Linux e2e green in CI;
   docs cover all three platforms.*

## Risks

- **Named-pipe security semantics differ from `peer_cred`** — mitigated by
  per-user pipe naming + explicit ACL, covered by M1 tests.
- **Windows file locking** breaks assumptions beyond self-update (e.g. config
  save while the daemon holds the file open). `save_atomic()` gets
  replace-existing rename semantics verified on Windows with concurrent-access
  tests in M1; if `std::fs::rename` proves insufficient, fall back to a
  retry-with-backoff swap.
- **TOCTOU protection is weaker on Windows** (no openat2). Accepted and
  documented; handle re-verification narrows but does not close the gap.
- **conpty PTY behavior differs from Unix PTYs** (already abstracted by
  `portable_pty`); PTY-dependent jobs are exercised in M1 integration tests.

## Out of Scope

- winget/scoop/MSI packaging, code signing.
- Windows service registration / auto-start (future work).
- Full Unix-mode→ACL mapping for FileChmod.
- Hub/dashboard/deploy tooling (server-side, Linux containers by design).
- musl / armv7 Linux targets (optional follow-up).
