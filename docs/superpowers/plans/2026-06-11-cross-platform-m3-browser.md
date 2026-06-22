# M3 — Browser Automation on Windows Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The browser-automation chain (Node runtime install → playwright-cli install → browser detection → runtime spawn) works natively on Windows, the last client-side bash spawns (`setup-browser.sh`) are replaced by Rust calls, and a Windows CI job proves the chain end-to-end. Exit (spec M3): browser setup chain green on a Windows runner.

**Architecture:** All work happens inside the plugin-runtime-era modules (`browser_setup/{node,playwright}.rs`, `plugin_runtime/runtime_dir.rs`, `browser.rs`) per the spec's "M3 prerequisite — replan against the plugin runtime" note. Spec: `docs/superpowers/specs/2026-06-11-cross-platform-windows-linux-design.md`.

**Key platform facts (verified against nodejs.org dist layout):**
- Windows Node dist = `node-v{V}-win-{arch}.zip`; inside: `node-v{V}-win-x64/` containing `node.exe`, `npm.cmd`, `npx.cmd`, `node_modules/npm/...` — **no `bin/` dir, no `npm.exe`**.
- `npm` on Windows is NOT an exe: invoke as `node.exe {node_dir}/node_modules/npm/bin/npm-cli.js` (CreateProcess can't exec `.cmd` directly).
- `npm install -g --prefix {P}` on Windows drops shims at `{P}\playwright-cli.cmd` + code at `{P}\node_modules\...` (unix: `{P}/bin/playwright-cli`). Spawn the installed CLI via `node.exe {prefix}\node_modules\@playwright\cli\...` entry JS or the unix bin path — resolve per-platform.
- `browser.rs:460` builds PATH with `':'` (broken on Windows); `plugin_runtime::path_env::prepend_path_dirs` already does it right — use it.
- Browser detection (`browser_detect.rs`) already has correct Windows Chrome/Edge paths.

**Dev-loop:** macOS dev box. ahandd cannot be cross-checked for Windows locally (lzma-sys); keep `#[cfg(windows)]` code review-clean and let CI verify. windows-latest runners ship Chrome+Edge AND have network access to nodejs.org/npm registry — the T5 e2e job is the authoritative gate.

---

## File Structure

```
Modify:
  crates/ahandd/src/browser_setup/node.rs        # zip download/extract + layout normalization (Windows)
  crates/ahandd/src/browser_setup/playwright.rs  # npm invocation via node.exe npm-cli.js; CLI resolution
  crates/ahandd/src/plugin_runtime/runtime_dir.rs# npm invocation spec (program+args), windows layouts
  crates/ahandd/src/browser.rs                   # PATH via path_env; .exe fallback
  crates/ahandd/src/plugin_runtime/activation.rs # only if 'shell' dep blocks windows (verify first)
  crates/ahandctl/src/browser_init.rs            # native: call ahandd::browser_setup
  crates/ahandctl/src/admin.rs                   # SSE route: ProgressEvent → SSE adapter (no bash)
  crates/ahandctl/src/upgrade/assets.rs          # stop shipping setup-browser.sh (native init replaces)
  scripts/dist/setup-browser.sh                  # deprecation header
  .github/workflows/client-ci.yml or new job     # T5 browser-chain e2e on windows-latest (+ubuntu)
  Cargo.toml(s)                                  # + zip crate (ahandd)
Docs: README browser section; spec M3 Completion Record.
```

Task order: 1 → 2 → 3 → 4 → 5 → 6. (2 depends on 1's layout decisions; 3/4 independent after 2; 5 is the gate.)

---

### Task 1: Windows Node install — zip download, extraction, layout normalization

**Goal:** `browser_setup::node::ensure()` installs a working Node runtime on Windows.

**Files:** `crates/ahandd/src/browser_setup/node.rs`, `crates/ahandd/src/plugin_runtime/runtime_dir.rs`, `crates/ahandd/Cargo.toml` (+ `zip = "2"`, default features minus what's unneeded; check crate API)

**Acceptance Criteria:**
- [ ] `platform_info()` → archive format decision: unix `.tar.xz` (unchanged), windows `.zip`
- [ ] Windows extraction via `zip` crate with the SAME safety properties as the tar path: strip first component, reject entries escaping the root (path-traversal guard — component-level, mirroring M2's `guard_path_traversal` semantics), preserve nothing weird (no symlink entries expected in zip; reject if encountered)
- [ ] **Layout normalization:** after extraction the install MUST satisfy `RuntimeDirs` expectations. Decide and implement ONE of: (a) normalize into `node/bin/` (move `node.exe`, keep `node_modules` adjacent at `node/`), updating nothing else; or (b) teach `RuntimeDirs::node_bin/npm_bin` windows-specific layouts. RECOMMENDATION: (a) — create `node/bin/node.exe` (move) + keep `node/node_modules/` (npm code) so `node_bin()` Just Works; document the normalized layout in a module comment. `npm_bin()` is DELETED/replaced in Task 2 (no npm.exe exists).
- [ ] `local_node_bin` exists post-install on windows (the ensure() verification passes)
- [ ] Unix flow byte-identical (existing tests untouched & green)
- [ ] Tests: zip-extraction unit tests run cross-platform (build a small in-memory zip fixture with the versioned-dir layout; assert normalization + traversal rejection + reject-symlink-entry); cfg(windows) assertions for final layout

**Verify:** `cargo test -p ahandd browser_setup && cargo test -p ahandd --bin ahandd node && cargo clippy -p ahandd --all-targets -- -D warnings && cargo fmt -p ahandd -- --check && cargo check --workspace`

Commit: `feat(browser): Windows Node runtime install (zip + layout normalization)`

---

### Task 2: npm + playwright-cli invocation, Windows-aware

**Goal:** npm runs on Windows (via `node.exe npm-cli.js`), playwright-cli installs and is spawnable.

**Files:** `crates/ahandd/src/plugin_runtime/runtime_dir.rs`, `crates/ahandd/src/browser_setup/playwright.rs`

**Acceptance Criteria:**
- [ ] `RuntimeDirs` gains `npm_invocation() -> (PathBuf /*program*/, Vec<OsString> /*leading args*/)`: unix → (`node/bin/npm`, []); windows → (`node/bin/node.exe`, [`{node_dir}/node_modules/npm/bin/npm-cli.js`]). Old `npm_bin()` removed or delegating (update all callers — grep). Unit tests both shapes.
- [ ] `spawn_npm_with_progress` uses `npm_invocation()` (program + leading args before npm args). Existing unix tests stay green (the fake-npm-script tests may need to adapt to the invocation seam — keep them honest: they should drive the SEAM, e.g. inject a fake invocation, not assume npm is a single binary).
- [ ] playwright-cli install path (`npm install -g --prefix <node_dir>` or whatever the current flow uses — READ it): confirm where the CLI lands per-platform and make `RuntimeDirs::playwright_cli_invocation()` (same program+args pattern): unix → (`node/bin/playwright-cli`, []); windows → (`node.exe`, [resolved CLI JS entry]). Resolve the entry script path from the npm global prefix layout (`{prefix}\node_modules\@playwright\cli\<bin entry>` — read package.json bin mapping at install time or hardcode the known entry with a fallback error). `playwright_cli_bin()` callers migrate.
- [ ] Post-install verification (the FAILED-to-install check) uses the invocation, not bare path existence, on windows
- [ ] Tests: invocation-shape unit tests (both platforms via cfg); playwright install flow against the existing mock-npm fixture still green on unix

**Verify:** `cargo test -p ahandd browser_setup && cargo test -p ahandd --bin ahandd playwright && cargo clippy -p ahandd --all-targets -- -D warnings && cargo fmt -p ahandd -- --check`

Commit: `feat(browser): Windows-aware npm and playwright-cli invocation`

---

### Task 3: Runtime spawn fixes in browser.rs

**Goal:** The daemon launches playwright-cli correctly on Windows at job time.

**Files:** `crates/ahandd/src/browser.rs`

**Acceptance Criteria:**
- [ ] `build_env_vars` PATH construction uses `plugin_runtime::path_env::prepend_path_dirs` (kills the `':'` literal at ~460)
- [ ] `binary_path`/spawn migrates to `playwright_cli_invocation()` from Task 2 (configured `binary_path` override still wins, treated as a direct program); fallback bare `"playwright-cli"` becomes platform-aware via `ahand_platform::paths::exe_name`
- [ ] Browser process kill path verified windows-safe (READ the kill/cleanup code; if it uses unix-only signals, route through `ahand_platform::process`)
- [ ] Tests: env-construction unit test asserting platform-correct separator (parse with `std::env::split_paths`); spawn-shape test via the invocation seam

**Verify:** `cargo test -p ahandd browser && cargo clippy -p ahandd --all-targets -- -D warnings && cargo fmt -p ahandd -- --check`

Commit: `fix(browser): platform-correct PATH and CLI invocation at runtime`

---

### Task 4: Native browser-init everywhere (kill the last bash spawns)

**Goal:** `ahandctl browser-init` and the admin panel SSE route call Rust directly; setup-browser.sh is legacy-only.

**Files:** `crates/ahandctl/src/browser_init.rs`, `crates/ahandctl/src/admin.rs`, `crates/ahandctl/src/upgrade/assets.rs`, `scripts/dist/setup-browser.sh`, `crates/ahandd/src/plugin_runtime/activation.rs` (conditional)

**Acceptance Criteria:**
- [ ] `browser_init.rs::run(force)` calls the ahandd lib browser_setup entry points directly (node::ensure → playwright::ensure with a ProgressEvent→stdout printer; `--force` maps to the existing force/clean semantics — READ browser_setup::mod for the canonical orchestration fn; if none exists, add `pub async fn run_all(force, progress) -> Result<Vec<CheckReport>>` in ahandd browser_setup)
- [ ] `admin.rs` SSE route: replace the bash pipeline with the same orchestration, adapting `ProgressEvent` → the existing SSE JSON line format (`{"line": ...}` + final `{"status","exit_code"}`) so the admin SPA needs no changes (KEEP the wire format byte-compatible — read how the SPA consumes it before changing anything)
- [ ] `upgrade/assets.rs`: remove the setup-browser.sh download/install steps (steps 4 & 10) — native init replaces the script; remove the now-dead `info.browser` usage or keep version resolution for display only (decide, document)
- [ ] `scripts/dist/setup-browser.sh`: deprecation header (kept for legacy installs)
- [ ] Activation: verify whether `browser-playwright-cli` plugin's dependency list (e.g. `["shell","node"]`) blocks Windows activation; if "shell" is satisfied cross-platform already (it is the exec capability, not bash), change nothing — RECORD the finding either way
- [ ] Tests: browser_init smoke (force=false on empty tempdir env → reports propagate errors cleanly — gate network-dependent paths; unit-test the ProgressEvent→SSE adapter (pure fn) incl. JSON escaping parity with the old format

**Verify:** `cargo test -p ahandctl -p ahandd && cargo clippy -p ahandctl -p ahandd --all-targets -- -D warnings && cargo fmt -p ahandctl -p ahandd -- --check && cargo check --workspace`

Commit: `feat(browser): native browser-init in ahandctl and admin panel (no bash)`

---

### Task 5: CI — browser-chain e2e on windows-latest (+ ubuntu) — THE M3 GATE

**Goal:** CI proves: Node zip install → npm → playwright-cli install → detection → doctor, on a real Windows runner.

**Files:** `.github/workflows/client-ci.yml` (new job) or a new workflow file (judgment: separate `browser-e2e` job in client-ci with `paths` guard is fine)

**Acceptance Criteria:**
- [ ] Job `browser-e2e` on `windows-latest` AND `ubuntu-latest`: build ahandd (debug), run its browser-init entry (the ahandd `BrowserInit` subcommand exists — `ahandd browser-init`; READ its clap def) with real network (nodejs.org + npm registry), then `ahandd browser-doctor` and assert success + expected report lines (node OK, playwright-cli OK, system browser found — Chrome preinstalled on both runner images)
- [ ] Reasonable timeout (~10 min) and retry-once on pure-download flakes (judgment)
- [ ] Optional stretch (only if cheap): a single real browser round-trip via playwright-cli (open about:blank / version probe) — if flaky, doctor-level assertion is the M3 bar; record the decision
- [ ] Iterate to green on BOTH OSes (budget 6 rounds; same fix policy as M1/M2: real fixes, no masking)

**Verify:** PR checks green incl. the new job.

Commit: `ci(browser): end-to-end browser-chain job on windows and ubuntu`

---

### Task 6: Docs + spec M3 completion record

**Files:** `README.md` (browser section: Windows supported note replacing the M3 placeholder), spec (M3 Completion Record mirroring M1/M2 style: landed/deviations/known gaps; M2-manual-list item for browser on real hardware if anything CI can't prove).

**Verify:** docs-only; claims checked against final code.

Commit: `docs: M3 browser automation documentation and completion record`

---

## Self-Review (plan-time)

- **Spec coverage (M3):** Node zip layout ✅(T1) npm/.cmd invocation ✅(T2) PATH `':'` fix ✅(T3) browser detection (already correct, verified in CI) ✅(T5) setup-browser absorption into Rust ✅(T4) "browser jobs pass on a Windows runner" ✅(T5 gate).
- **Plugin-runtime alignment:** all changes land in plugin-runtime-era modules; `runtime_dir.rs` local `exe_name` duplication with `ahand_platform::paths::exe_name` — T2 should unify (use the platform crate's) — added to T2 scope implicitly via runtime_dir edits; make it explicit: **T2 also replaces the local `exe_name` with `ahand_platform::paths::exe_name`** (ahandd already depends on ahand-platform).
- **Placeholder scan:** none; READ-first instructions where layouts must be confirmed in code.
- **Known risk:** npm global-prefix layout on Windows is the least-verified assumption — T2 instructs reading the actual install flow + a fallback error with remediation text; T5's real CI run is the ground truth.
