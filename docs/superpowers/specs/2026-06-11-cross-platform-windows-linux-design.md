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
| `ipc` | `UnixListener`/`UnixStream`, socket at `~/.ahand/ahandd.sock`, `peer_cred` check, `0o660` | `tokio::net::windows::named_pipe`, pipe name `\\.\pipe\ahandd-<username>`, **explicit owner-only DACL** `D:P(A;;GA;;;OW)(A;;GA;;;SY)(A;;GA;;;BA)` enforced in `create_secured_pipe` (M4; replaces `peer_cred`. The M1-era "default per-user ACL" was hardened to this code-enforced descriptor — see M4 Completion Record) |
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
  are invoked on Windows via `node.exe <js-entrypoint>` (npm-cli.js path, or
  the `@playwright/cli` package.json `"bin"`-resolved JS entry),
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

## M3 Completion Record (2026-06-11)

M3 landed on `feat/cross-platform-m3` (plan:
`docs/superpowers/plans/2026-05-13-ahand-plugin-runtime-stage1.md`).

**What landed:**

- **Windows Node install** — `browser_setup/node.rs` downloads the official
  Windows `.zip` distribution from nodejs.org, extracts it with
  traversal/symlink guards (component-level `ParentDir`/`RootDir`/`Prefix`
  rejection, symlink entry rejection), then normalises the flat zip layout
  (moving `node.exe` from the archive root into `bin/node.exe`) so that
  `RuntimeDirs::node_bin()` — which always returns `<node_dir>/bin/node[.exe]`
  — resolves correctly on all platforms without platform-specific branches in
  `RuntimeDirs`.
- **npm / playwright-cli invocation seams** — `RuntimeDirs::npm_invocation()`
  returns `(node.exe, [npm-cli.js])` on Windows (program is `node.exe`, leading
  arg is the `node_modules/npm/bin/npm-cli.js` path). No `npm.exe` assumption.
  `RuntimeDirs::playwright_cli_invocation()` on Windows resolves the JS entry
  from the `@playwright/cli` `package.json` `"bin"` field (string or object
  form), falling back to the conventional `cli.js` path; returns `Err` (not a
  bare fallback) when neither exists, so callers surface `CheckStatus::Missing`
  cleanly.
- **`browser.rs` platform PATH via `path_env`** — `build_env_vars()` uses
  `crate::plugin_runtime::path_env::prepend_path_dirs` (which calls
  `std::env::join_paths` internally, using `;` on Windows, `:` on Unix) to
  prepend the managed `node/bin` directory to PATH. The three-priority CLI
  chain (`cli_invocation_with`) is: (1) configured override → direct program,
  no leading args; (2) managed runtime `playwright_cli_invocation()` — Ok only
  when the binary / entry JS actually exists on disk; (3) bare
  `exe_name("playwright-cli")` fallback relying on system PATH. The chain is
  symmetric on error: managed-missing falls through to bare fallback, not a
  hard error.
- **Native `ahandctl browser-init`** — `crates/ahandctl/src/browser_init.rs`
  calls `ahandd::browser_setup::run_all` directly (no bash subprocess, no
  script file on disk required). The progress callback uses
  `format_progress_line` (shared with `ahandd` CLI) for human-readable output.
- **Admin SSE** — `ahandctl/src/admin.rs` `browser_init_stream` emits
  `ProgressEvent`s as `{"line":"<escaped>"}` SSE events using
  `progress_event_to_sse_line` (which delegates to `format_progress_line`
  then serialises via `serde_json::json!` for correct JSON escaping including
  embedded newlines from multi-line anyhow error chains). Wire format is
  byte-compatible with the old bash-stream implementation for single-line
  messages; serde correctly handles the multiline case the old hand-rolled
  `replace('\\', …).replace('"', …)` did not. The terminal event is
  `{"status":"done|error","exit_code":N}` (also serialised via serde_json).
- **Shared progress formatter** — `browser_setup/progress_format.rs` is the
  single rendering surface for both CLI and SSE. Rules: `Phase::Done` → `✓`,
  `Phase::Failed` → `✗`, `Phase::Log` with `LogStream::Stderr` →
  `[stderr] <msg>`, all other phases → message unchanged.
- **`setup-browser.sh` deprecated** — header updated; no longer shipped by
  native `ahandctl upgrade` (`upgrade/assets.rs` steps 4 and 10 removed).
  `install.sh` / release pipelines still distribute it for legacy shell-based
  installs. The file is kept in `scripts/dist/` for those paths only; no new
  code should depend on it — all new browser setup must go through
  `ahandctl browser-init` (native Rust).
- **`browser-e2e` CI job green on ubuntu-latest + windows-latest** — real
  nodejs.org / npm downloads, `ahandd browser-init` (with one retry on flake),
  then `ahandd browser-doctor` asserting all-pass (`Node.js:`,
  `playwright-cli:`, `System Browser:`, `all checks passed`). Chrome
  auto-detected on both runners without manual install.

**Deviations from the M3 spec:**

No material deviations. `Phase::Failed` was added to the `Phase` enum as a
presentation contract addition (emitted by the failure-wrapper path so
formatters can render `✗` without inspecting the `Result`); this is an
additive change to `types.rs` that does not break any callers.

**M4 backlog:** unchanged from the M1 record. The browser chain no longer
contributes any new M4 items beyond the existing list (TOCTOU, symlink ops,
`sanitize_env` Windows blocklist, ACL chmod, shell e2e un-gating,
named-pipe explicit security descriptor).

## M4 Completion Record (2026-06-12)

M4 work began on `feat/cross-platform-m4`. The first task was un-gating the
shell-execution / `sanitize_env` test suite on the real `windows-latest` lane
and fixing the failures the live Windows runner surfaced.

**What landed:**

- **`sanitize_env` strip tests are now platform-robust.** `sanitize_env` seeds
  its result from the daemon's own process env (`env::vars()`), then *skips*
  blocked keys present in the override map. A blocked key already in the base
  env (on Windows, `PATHEXT` and `COMSPEC` are always present) therefore keeps
  the daemon's own value — the security property is intact (a cloud override
  cannot *change* `PATHEXT`/`COMSPEC`), but the old test assertion
  (`!result.contains_key(KEY)`) was wrong and failed on the real Windows lane.
  All blocked-key strip tests now assert the true invariant — a malicious
  override value never *wins* (`result.get(KEY) != Some(malicious)`) — which is
  correct on every platform. (The Unix-only keys like `NODE_PATH`/`BASH_ENV`
  were also converted; their old `!contains_key` only *happened* to hold
  because those keys aren't in a normal base env.)

- **Strict-mode marker command is `cmd.exe`-safe.** The real shell-job e2e tests
  (`strict_mode_*`) run `cmd /C <write_marker_command>` on Windows. The marker
  command had a quoted path (`echo X> "<path>"`); the whole raw command is
  passed to `cmd.exe /C` as a SINGLE arg by `tokio::process::Command`, which
  serialises it with MSVC-CRT quoting (`"` → `\"`). `cmd.exe` does not parse
  `\"` (it uses `""`/`^`), so the quoted path was mangled and the redirect wrote
  to the wrong target → the marker file never appeared at the asserted path. The
  test command is now unquoted (`echo X><path>`); `unique_output_path` builds the
  path under `temp_dir()`, which is space-free on the CI runner
  (`C:\Users\runneradmin\AppData\Local\Temp`), so an unquoted path is safe.

**New M4 backlog item — production `cmd /C` quoted-argument gap (Windows):**

The fix above is scoped to the *test* command, but the root cause is a real
(edge-case) production concern. In `openclaw/handler.rs::run_command`, the shell
path spawns `Command::new(cmd.exe).arg("/C").arg(&raw_command)` — the entire
cloud-sent `rawCommand` string is passed as ONE argument. Rust's `Command`
applies MSVC-CRT argument escaping (escapes `"` as `\"`), which is incompatible
with `cmd.exe`'s own quote parsing (cmd uses `""` and `^`, and treats `\"`
literally). So any cloud-sent `rawCommand` that contains double quotes (e.g. to
handle a path with spaces) is corrupted on Windows before `cmd.exe` ever sees
it. Out of scope for the current M4 test-stabilisation task; tracked here as a
backlog item. Likely fix: bypass the CRT quoter with
`std::os::windows::process::CommandExt::raw_arg` and build a `cmd.exe`-correct
command line by hand (escaping `"` as `""`/`^` per cmd rules), or document that
`rawCommand` on Windows must already be a cmd-valid one-liner. Until then,
quoted-argument `rawCommand`s are unsupported on Windows.

**Full task ledger (all 7 M4 tasks landed on `feat/cross-platform-m4`).** The
test-stabilisation pass above was the entry point; the bulk of the security
hardening landed in the same PR. Final state of each plan task
(`docs/superpowers/plans/2026-06-12-cross-platform-m4-security.md`):

- **T1 — native symlink creation** (`fs_ops.rs::handle_create_symlink`): Windows
  arm via `symlink_file`/`symlink_dir` chosen by target type, `ERROR_PRIVILEGE_NOT_HELD`
  (1314) mapped to a clear error, target policy-checked. A nonexistent target
  falls back to `symlink_file` (dangling link).
- **T2 — ACL chmod** (`fs_ops.rs::handle_chmod`, `Permission::Windows`): uses
  `SetNamedSecurityInfoW` + `PROTECTED_DACL_SECURITY_INFORMATION` for a true
  owner-only DACL replacement (NOT `icacls` — `/inheritance:r` leaves explicit
  ACEs; see `SECURITY-HIGH#1`). Recursive apply self-walks and skips reparse
  points (NOT `icacls /T`, which follows junctions out of the allowlist —
  `SECURITY-HIGH#2`). Principals resolve via `LookupAccountNameW`→SID, never raw
  SDDL interpolation.
- **T3 — hardened `sanitize_env`** (`handler.rs`): PATH overrides validated
  prepend-only via `env::split_paths`; `BLOCKED_KEYS` adds `PATHEXT`/`COMSPEC`
  (+ the existing loader/lookup keys); `BLOCKED_PREFIXES` = `DYLD_`, `LD_`. Each
  entry carries a concrete injection-mechanism comment — no speculative padding.
- **T4 — case-correct policy matching** (`policy.rs::glob_match`): Windows folds
  both pattern and path with Unicode `to_lowercase` and matches with
  `case_sensitive: false`; Unix stays case-sensitive. Malformed patterns fall
  back to literal (case-folded on Windows) equality — fail-closed for denylists.
- **T5 — un-gated strict-mode shell e2e** (`handler.rs`): the `strict_mode_*`
  approval-invariant tests now run on both platforms via a `cmd.exe`-safe marker
  command (see backlog note above for the residual production quoting gap).
- **T6 — explicit named-pipe security descriptor** (`ahand-platform/src/ipc.rs`):
  `create_secured_pipe` applies `D:P(A;;GA;;;OW)(A;;GA;;;SY)(A;;GA;;;BA)` to all
  three `ServerOptions` create sites via `windows-sys`, with an RAII `LocalFree`
  guard; `pipe_sddl()` is unit-pinned.
- **T7 — Windows security regression suite** (`tests/file_ops.rs`): Windows
  analogues for the `cfg(unix)` symlink/junction-escape, ACL, and case-denylist
  tests; fixtures go through `canonicalize_simplified` (no raw `.canonicalize()`).
  The openat2-only tests stay gated and annotated as having no Windows
  equivalent (the deferred TOCTOU window — see scope boundary in the plan).

**Test-completeness follow-up (same PR):** a coverage audit added regression
tests for the three load-bearing-but-untested security branches — the
`DYLD_`/`LD_` loader-injection prefix blocks, the case-insensitive key
normalization (lowercase blocked-key bypass), and the `glob_match` malformed-
pattern fail-closed fallback.
