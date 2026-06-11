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
| `shell` | `SHELL` env → fallback `/bin/sh`; `sh -c <cmd>` | `COMSPEC` env → fallback `cmd.exe`; `cmd /C <cmd>` (PowerShell override: future option, not in M1) |
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

- **OpenClaw array commands (2+ elements) spawn directly, never via shell**:
  argv boundaries pass verbatim to `Command::new(cmd[0]).args(cmd[1..])`, so
  `& | > ^ %` in arguments cannot escape into shell syntax (closes a cmd.exe
  allowlist-bypass vector). Single-element and raw-string commands keep using
  the platform shell.
- **FileChmod (Unix mode) returns an explicit unsupported error on Windows**
  (no silent ignore). `FileMkdir.mode` is documented as Unix-only. A full
  Windows ACL mapping (proto already has ACL fields) is a separate future project.
- **TOCTOU protection on Windows is NOT implemented in M1** (M4 backlog):
  the planned `GetFinalPathNameByHandle` on-open re-verification has not been
  built; Windows file ops run without the openat2/symlinkat protection Unix
  has. Accepted M1 gap, recorded in the M4 backlog.
- **The daemon runs as a detached process on Windows** — no Windows service
  registration in v1 (parity with macOS, which has no launchd integration
  either). Auto-start is future work.
- **`sanitize_env` Windows blocklist is an M4 item** (not implemented in M1):
  it will block loader/lookup injection vectors (e.g. `PATHEXT` tampering) and
  compare PATH overrides via `env::split_paths` instead of `':'` string checks.
- Symlink file ops on Windows return an explicit unsupported error in M1
  (`TODO(M4)`: implement via `std::os::windows::fs::symlink_file/dir` with
  developer-mode detection; never silently skip).
- **Policy patterns must be written as plain paths** (`C:\Users\...`, never a
  `\\?\` verbatim prefix). Paths that dunce cannot simplify — verbatim-UNC
  (`\\?\UNC\...`) network shares and over-MAX_PATH paths — keep their prefix
  and therefore fail allowlist matching: such ops **deny by default** in M1
  (fail-closed; recorded decision, not a bug). Windows glob matching is
  case-sensitive while the filesystem is not — case-mismatch hardening and
  tests land in M4.
- **Secret files** (`device identity ×2, pairing state, exec approvals`) are
  written via `ahand-platform::secure_file`: each call creates a unique tmp
  `.{name}.{pid}.{counter}.tmp` with `O_CREAT|O_EXCL` and pre-write 0o600 on
  Unix / post-write owner-only `icacls` ACL on Windows, then renames over the
  target. Before creating the tmp, sibling stale tmps (same prefix/suffix) are
  swept best-effort. Concurrent writers use unique tmp names; last complete
  write wins; stale tmps are swept on the next write. Accepted M1 windows: the
  Windows tmp briefly carries default ACLs inside the (user-profile) target
  directory; Windows replace is remove-then-rename, so concurrent readers can
  observe a missing file. chmod/ACL failure is a hard error (fail-closed;
  previously chmod failures were silently ignored). Pulled forward from M4.

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

## M1 Completion Record (2026-06-11)

M1 landed on `feat/cross-platform-m1` (plan:
`docs/superpowers/plans/2026-06-11-cross-platform-m1-windows-core.md`).
Client CI green on ubuntu/macos/windows (fmt, clippy `-D warnings`, full test
suite); release matrix builds all 5 targets including
`ahandd-windows-x64.exe`/`ahandctl-windows-x64.exe`.

**Deviation reconciled:** the `restart` row above originally called for a
platform module; the implementation keeps rename-aside + restart logic inline
in `updater.rs` with `#[cfg]` arms (plan Task 9 decision). If M2 grows more
process-swap logic, extract `process::restart_in_place()` then.

**M3 prerequisite — replan against the plugin runtime.** The plugin-runtime
mechanism merged from main mid-M1 moved browser-binary management to
`plugin_runtime::RuntimeDirs` (`~/.cache/ahand-runtimes/...`); the
"Install / upgrade / browser-setup chain" section above still describes the
old `browser_setup` script paths. Consequences for M3: (1) the Windows Node
layout problem (zip archive, `node.exe` at root vs the `node/bin/<exe>`
layout `RuntimeDirs` assumes) must be fixed in the plugin-runtime
installers — normalize the layout at install time as already planned, but
the install code lives in plugin_runtime adapters now; (2) `path_env`
already uses `env::join_paths` + cfg-aware separators, which resolves the
browser PATH-`':'` audit finding; (3) `runtime_dir.rs` carries a local
`exe_name()` duplicating `ahand_platform::paths::exe_name` — unify during
M3. Rewrite the M3 task breakdown against plugin_runtime before starting.

**M2 must verify manually on a real Windows host** (green CI does not prove
these):

1. Live hub connection (real hub, not mock) reaches Online.
2. A real shell job (`cmd /C`) executes and returns output — the only shell
   e2e tests are `#[cfg(unix)]`-gated.
3. Named-pipe IPC on the default `\\.\pipe\ahandd-<user>` endpoint, including
   cross-user rejection.
4. `icacls` grant string on a domain-joined account (`DOMAIN\user`).
5. Self-update rename-aside against a RUNNING `ahandd.exe` + `.old` cleanup on
   restart.
6. `ahandctl stop` graceful/force path via taskkill.
7. The published windows-x64 release artifact actually launches.

**M4 backlog (from per-task reviews + final integration review):**

- Port `create_symlink` to Windows (`fs_ops.rs`, `TODO(M4)`).
- Implement Windows ACL chmod (`fs_ops.rs`, `TODO(M4)`).
- Un-gate / port the two `#[cfg(unix)]` shell-execution e2e tests in
  `openclaw/handler.rs` to `cmd /C`.
- Windows variants for the `#[cfg(unix)]` file-ops security regression suite
  (`io_safe` TOCTOU, symlink/junction escapes, mode bits → ACL asserts). When
  un-gating, route every fixture through
  `ahand_platform::paths::canonicalize_simplified` — raw `.canonicalize()` in
  test helpers reintroduces the `\\?\` landmine.
- `sanitize_env` Windows blocklist (spec M4 item, untouched in M1).
- Windows glob case-insensitivity hardening for policy matching.
- **Named-pipe explicit security descriptor** (Codex review finding, accepted
  M1 risk): the pipe is created with the DEFAULT Windows SD, whose DACL grants
  Everyone READ — on a multi-user Windows host another local user could open
  the pipe read-only and observe approval-broadcast envelopes (metadata leak;
  cannot submit jobs, which requires write access). Fix in M4 via
  `ServerOptions::create_with_security_attributes_raw` with an owner-only
  DACL; M2 manual verification must test cross-user open behavior on real
  hardware (already on the M2 list).
- Optional: unify `ahandctl admin`'s direct `ctrl_c()` onto
  `platform::signals`.

## M2 Completion Record (2026-06-11)

M2 landed on `feat/cross-platform-m2` (plan:
`docs/superpowers/plans/2026-06-11-cross-platform-m2-lifecycle-install.md`).

**What landed:**

- **Native `ahandctl upgrade`** — fully rewritten in Rust (`crates/ahandctl/src/upgrade/`); no shell subprocess. Flow: resolve latest release via GitHub API → download binaries + optional checksum file → SHA-256 verify before any install → stop daemon → rename-aside self-swap via `ahandd::updater::swap_binary_into` (works on Windows: rename running binary to `.old`, place new binary, clean `.old` on next start) → admin-SPA traversal-guarded tar extraction → write version marker. `--check` mode queries versions and prints current vs. available without installing.
- **`swap_binary_into` generalization** — `ahandd::updater::swap_binary_into` is now the shared primitive used by both `ahandd` self-update and the new `ahandctl upgrade` path. Works on all three platforms.
- **`AHAND_DIR` / `AHAND_VERSION` env-var support** in `ahandctl upgrade` — `upgrade::resolve_ahand_home()` checks `AHAND_DIR` first; `run()` checks `AHAND_VERSION`. Mirrors `install.sh` / `install.ps1` behaviour.
- **`scripts/dist/install.ps1`** — PowerShell one-liner installer for Windows. Parameters: `ApiBase`, `DownloadBase`, `InstallDir`, `Version`, `NoPathUpdate`. Idempotent user-PATH update (skips if already present). ARM64 gives a clear error (no artifacts yet). Minimum Windows 10 1803+ for in-box `tar`.
- **`test-dist-scripts.yml` `windows-install` job** — mocked e2e job on `windows-latest` (mock HTTP server via .NET `HttpListener`): first-run install, binary + version-file assertions, PATH idempotency (second run), checksum-mismatch negative test. Green.
- **`upgrade.sh` deprecated** — header updated; kept for legacy installs that pre-date native upgrade support.

**Deviations from the M2 spec:**

- Browser automation (`setup-browser.sh` chain) is intentionally untouched. The plugin-runtime replan note in the M1 record calls this out; the install.ps1 comment (`# intentionally omitted on Windows in M2`) matches. Browser support tracks to M3.
- `windows-arm64` install is blocked on missing release artifacts; `install.ps1` gives an explicit error rather than silently failing.

**Still pending — USER ACTION required:**

The M2 manual-verification checklist from the M1 Completion Record (items 1–7, "M2 must verify manually on a real Windows host") has not been run yet. These items require a real Windows host and remain the M2 exit gate before the branch is merged to main.

**Known issue (pre-existing, not introduced by M2):**

The `test` job in `test-dist-scripts.yml` (BATS, `ubuntu-latest` + `macos-latest`) has been stale-red on `main` since 2026-03-11, before M2 work began. This is under separate investigation; the `windows-install` job is independent and green.
