# M2 — Lifecycle & Install Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `ahandctl upgrade` runs natively on all three platforms (no bash), and Windows users get a PowerShell one-line installer; exit criterion: the PowerShell one-liner installs a working, upgradeable daemon.

**Architecture:** Upgrade logic moves from `scripts/dist/upgrade.sh` into Rust inside `ahandctl` (reusing `ahand_platform` process/paths helpers and `ahandd`'s download/checksum helpers, which `ahandctl` already links). A new `scripts/dist/install.ps1` mirrors `install.sh`. Spec: `docs/superpowers/specs/2026-06-11-cross-platform-windows-linux-design.md` (§ Install/upgrade chain; M1 Completion Record).

**Tech Stack:** Rust 2024, tokio, reqwest (via ahandd lib), tar+flate2 (admin-spa), PowerShell 5.1+ compatible script, GitHub Actions (test-dist-scripts mock framework).

**Already done in M1 (do NOT redo):** daemon start/stop/status native; release matrix ships `ahandd-windows-x64.exe`/`ahandctl-windows-x64.exe` + `checksums-rust-windows-x64.txt` (verify exact names in `.github/workflows/release-rust.yml` — an extraction note claimed `win-x64`, trust the file); rename-aside swap pattern exists in `ahandd::updater::install_binary_into`.

**Out of scope (M3):** `browser_init.rs` / `admin.rs` setup-browser.sh spawn sites (browser chain — M3 replans against plugin runtime). `admin.rs` SSE route stays bash in M2.

**Dev-loop:** macOS dev box; every task runs `cargo check --target x86_64-pc-windows-msvc -p ahand-platform` where platform code changes (ahandctl/ahandd can't cross-check locally — lzma-sys C dep; Windows truth = client-ci). PowerShell syntax-check locally via `pwsh -NoProfile -Command "[scriptblock]::Create((Get-Content -Raw scripts/dist/install.ps1)) | Out-Null"` if pwsh exists, else rely on CI.

---

## File Structure

```
Create:
  scripts/dist/install.ps1                  # Windows bootstrap installer
  crates/ahandctl/src/upgrade/mod.rs        # native upgrade engine (replaces upgrade.rs)
  crates/ahandctl/src/upgrade/release.rs    # GitHub releases API + version resolution
  crates/ahandctl/src/upgrade/assets.rs     # download/verify/extract/swap
Modify:
  crates/ahandctl/Cargo.toml                # + tar, flate2, sha2, reqwest? (prefer reusing ahandd's pub helpers)
  crates/ahandd/src/updater.rs              # pub(crate)→pub for download_binary/verify_checksum (+ lib re-export if needed)
  crates/ahandd/src/lib.rs                  # re-export check (updater already pub mod)
  crates/ahand-platform/src/paths.rs        # + release_suffix() helper ("darwin-arm64"/"linux-x64"/"windows-x64")
  .github/workflows/test-dist-scripts.yml   # + windows install.ps1 job
  scripts/dist/upgrade.sh                   # deprecation banner + fix SUFFIX bug (legacy path)
Delete:
  crates/ahandctl/src/upgrade.rs            # replaced by upgrade/ module
```

Task order: 1 → 2 → 3 → 4 (ps1, parallel-safe after 1) → 5 (CI) → 6.

---

### Task 1: `ahand_platform::paths::release_suffix()` + version-file helpers

**Goal:** One source of truth for release asset suffixes and the `~/.ahand/version` marker.

**Files:**
- Modify: `crates/ahand-platform/src/paths.rs`
- Test: inline

**Acceptance Criteria:**
- [ ] `release_suffix() -> &'static str`: `darwin-arm64`/`darwin-x64`/`linux-x64`/`linux-arm64`/`windows-x64` via `cfg!(target_os)`+`cfg!(target_arch)`; compile_error! on unsupported combos is NOT acceptable (other targets may build the crate) — return best-effort string built from `std::env::consts::{OS, ARCH}` mapped (`macos`→`darwin`, `x86_64`→`x64`, `aarch64`→`arm64`)
- [ ] `version_file(ahand_home: &Path) -> PathBuf` (= `ahand_home/version`), `read_version_marker(ahand_home) -> Option<String>` (trimmed), `write_version_marker(ahand_home, v) -> io::Result<()>`
- [ ] Tests: suffix matches the current platform's expected value (cfg-gated asserts); marker round-trip incl. whitespace trim; read on missing file → None

**Verify:** `cargo test -p ahand-platform paths && cargo check --target x86_64-pc-windows-msvc -p ahand-platform && cargo clippy -p ahand-platform --all-targets --target x86_64-pc-windows-msvc -- -D warnings`

**Steps:** TDD: tests first (compile-fail), implement, verify, commit
`feat(platform): release_suffix and version-marker helpers`.

NOTE: suffix MUST match `.github/workflows/release-rust.yml` matrix `suffix:` values — read that file first and pin the mapping in a comment.

---

### Task 2: Expose ahandd download/checksum helpers; native upgrade engine — release resolution

**Goal:** `ahandctl` can resolve current/latest versions and download release assets natively.

**Files:**
- Modify: `crates/ahandd/src/updater.rs` (make `download_binary`, `verify_checksum` `pub` with doc comments; keep signatures)
- Create: `crates/ahandctl/src/upgrade/mod.rs`, `crates/ahandctl/src/upgrade/release.rs`
- Modify: `crates/ahandctl/src/main.rs` (module path `mod upgrade;` stays valid — directory module replaces file)
- Delete: `crates/ahandctl/src/upgrade.rs`
- Test: inline + `crates/ahandctl/tests/upgrade_release.rs` (mock GitHub API via axum dev-dep, mirroring `ahandd/tests/file_ops_s3_write.rs` stub pattern)

**Acceptance Criteria:**
- [ ] `release.rs`: `ReleaseInfo { rust: Option<String>, admin: Option<String>, browser: Option<String> }`; `resolve_latest(api_base: &str) -> Result<ReleaseInfo>` parsing `/repos/team9ai/aHand/releases` JSON tag_names (`rust-v*`, `admin-v*`, `browser-v*`, first match each = latest); `resolve_target(version_override: Option<&str>, api_base) -> Result<ReleaseInfo>` (override pins all three, like `--version`/`AHAND_VERSION`)
- [ ] `current_version(ahand_home) -> String`: version marker → fallback `env!("CARGO_PKG_VERSION")` → "unknown" (NO ahandctl --version subprocess — we ARE ahandctl)
- [ ] `api_base` injectable (default `https://api.github.com`) so tests use a local stub; download base likewise injectable (`https://github.com/{repo}/releases/download`)
- [ ] `mod.rs`: `pub async fn run(check_only, target_version) -> Result<()>` — check mode prints current/latest/platform and "Update available"/"Already up to date" matching upgrade.sh UX; full mode delegates to Task 3's flow (stub allowed at this task's commit: `bail!("not yet implemented")` behind a `// Task 3` marker is FORBIDDEN by plan rules — instead structure commits so Task 2 lands check-mode fully working and the full-mode path returns the Task-3 flow function which Task 3 fills — acceptable: land `run` calling `perform_upgrade(...)` defined in assets.rs with a `todo-free` minimal body that errors with "full upgrade not yet available in this build; use --check" AND a test pinning that message, replaced in Task 3)
- [ ] AHAND_DIR env honored for ahand_home (default `~/.ahand`), AHAND_VERSION honored as version override
- [ ] Integration test: stub API returns fixture JSON → resolve_latest picks correct three versions; missing admin/browser tags → None; check-mode run() against stub prints "Already up to date" when marker equals latest

**Verify:** `cargo test -p ahandctl && cargo clippy -p ahandctl --all-targets -- -D warnings && cargo check --workspace`

Commit: `feat(ahandctl): native upgrade check via GitHub releases API`.

---

### Task 3: Native upgrade engine — download/verify/swap/finish

**Goal:** Full `ahandctl upgrade` parity with upgrade.sh on all platforms, Windows-safe.

**Files:**
- Create: `crates/ahandctl/src/upgrade/assets.rs`
- Modify: `crates/ahandctl/src/upgrade/mod.rs` (wire full flow), `crates/ahandctl/Cargo.toml` (+ `tar = "0.4"`, `flate2 = "1"`; reqwest/sha2 come via `ahandd` helpers)
- Test: extend `crates/ahandctl/tests/upgrade_release.rs` with full-flow test against local stub serving binaries+checksums+admin tar.gz

**Acceptance Criteria (parity with upgrade.sh, in order):**
- [ ] Download `checksums-rust.txt` (optional, tolerate 404), `ahandd-{suffix}[.exe]`, `ahandctl-{suffix}[.exe]` (required), `admin-spa.tar.gz` (optional if admin version), `setup-browser.sh` (optional if browser version, unix only)
- [ ] Verify sha256 against the checksum file entries `{bin}-{suffix}[.exe]` when present (reuse `ahandd::updater::verify_checksum`); mismatch = hard error before any install step
- [ ] Stop daemon via `crate::daemon::stop()` (NOT raw kill — reuses graceful/force logic)
- [ ] Swap binaries into `{ahand_home}/bin` with `.exe` awareness: ahandd via straight tmp+rename; **ahandctl swaps ITSELF while running** — on Windows rename the running `ahandctl.exe` aside to `.old` first (same pattern as `ahandd::updater::install_binary_into`; generalize: add `pub fn swap_binary_into(bin_dir, base_name, data) -> Result<()>` to `ahandd::updater` (rename-aside + unix 0o755 + cleanup fn) and reuse for both binaries; keep existing `install_binary_into` delegating to it; ahandctl startup gets a `cleanup_old_binary`-equivalent call for `ahandctl.exe.old` in main())
- [ ] Unix: 0o755 on installed binaries; macOS: `xattr -d com.apple.quarantine` best-effort
- [ ] admin-spa: clear `{ahand_home}/admin/dist` contents then extract tar.gz there (tar+flate2; path-traversal safe — reject entries escaping dist via `Path::components` check on each entry)
- [ ] Write version marker last; print upgrade.sh-style summary + "Restart the daemon: ahandctl restart"
- [ ] Full-flow integration test (unix CI): stub serves tiny fake binaries + matching checksums + a small tar.gz; run full upgrade against `AHAND_DIR=tempdir`; assert binaries installed+executable, dist extracted, version marker written, daemon-stop tolerates not-running. Bad-case tests: checksum mismatch aborts BEFORE install; tar entry `../escape` rejected; required asset 404 → clear error
- [ ] `upgrade.sh` gets a deprecation header comment + the `SUFFIX="${OS}-${SUFFIX}"` bug fixed to `"${OS}-${ARCH}"` (legacy installs still call it until they upgrade once)

**Verify:** `cargo test -p ahandctl -p ahandd && cargo clippy -p ahandctl -p ahandd --all-targets -- -D warnings && cargo check --workspace && cargo fmt -p ahandctl -p ahandd -- --check`

Commit: `feat(ahandctl): full native upgrade (download/verify/swap/admin-spa) with Windows self-swap`.

---

### Task 4: `scripts/dist/install.ps1`

**Goal:** `irm <url>/install.ps1 | iex` installs ahandd/ahandctl (+optional admin panel) on Windows.

**Files:**
- Create: `scripts/dist/install.ps1`

**Acceptance Criteria (parity with install.sh):**
- [ ] Env overrides: `AHAND_VERSION` (pin all), `AHAND_DIR` (default `$env:USERPROFILE\.ahand`)
- [ ] Resolve versions from `https://api.github.com/repos/team9ai/aHand/releases` (Invoke-RestMethod; pick first `rust-v*`/`admin-v*`/`browser-v*` tags)
- [ ] Download `ahandd-windows-x64.exe`, `ahandctl-windows-x64.exe` (exact names from release-rust.yml; arch from `$env:PROCESSOR_ARCHITECTURE`, AMD64→x64; bail clearly on ARM64 until artifacts exist), checksum file optional verify via `Get-FileHash -Algorithm SHA256`
- [ ] Install to `$AHAND_DIR\bin\ahandd.exe` / `ahandctl.exe`; admin-spa.tar.gz optional → extract with `tar -xzf` (Win10 1803+ ships bsdtar) into `$AHAND_DIR\admin\dist`
- [ ] Write `$AHAND_DIR\version`
- [ ] PATH: append `$AHAND_DIR\bin` to USER Path via `[Environment]::SetEnvironmentVariable('Path', ..., 'User')` ONLY if absent; print note that new terminals pick it up; never duplicate entries
- [ ] `-ErrorActionPreference Stop`; works under PowerShell 5.1 AND pwsh 7 (no 7-only syntax: no `&&`, no ternary)
- [ ] Final output: install dir, next steps (`ahandctl configure`, `ahandctl start`) mirroring install.sh

**Verify:** `pwsh -NoProfile -File scripts/dist/install.ps1 -?` parse check if pwsh present locally; authoritative check is Task 5's CI job.

Commit: `feat(dist): PowerShell one-line installer for Windows`.

---

### Task 5: CI — install.ps1 mocked test on windows-latest

**Goal:** test-dist-scripts.yml proves install.ps1 end-to-end against a mocked release.

**Files:**
- Modify: `.github/workflows/test-dist-scripts.yml`

**Acceptance Criteria:**
- [ ] READ the existing workflow's mock approach first; add a `windows-install` job on `windows-latest`: start a local static file server (PowerShell or `python -m http.server`) serving fake `ahandd-windows-x64.exe`/`ahandctl-windows-x64.exe` + a releases JSON; point install.ps1 at it via parameter/env override (add a `-ApiBase`/`-DownloadBase` override to install.ps1 if the existing mock pattern needs it — mirror however install.sh tests mock GitHub; if install.sh tests mock via PATH-stubbed curl, the ps1 equivalent is param overrides, document the asymmetry)
- [ ] Asserts: binaries land in `$env:AHAND_DIR\bin`, version file written, USER Path updated exactly once across two runs (idempotency), second run with same version prints up-to-date behavior for installer (re-install is fine; no duplicate PATH)
- [ ] Job runs on PRs touching `scripts/dist/**` or the workflow

**Verify:** push branch → workflow_dispatch or PR run green (this workflow has no branch push trigger — confirm trigger setup mirrors client-ci pull_request paths; temporary branch trigger allowed with `# TEMPORARY` marker, removed before merge).

Commit: `ci(dist): windows install.ps1 mocked end-to-end test`.

---

### Task 6: Docs + spec completion record

**Goal:** README Windows install section; spec M2 record.

**Files:**
- Modify: `README.md` (Quick Start: add Windows PowerShell one-liner alongside curl|bash; Upgrade section: `ahandctl upgrade` now native), spec (M2 completion record: what landed, what the user must verify on real hardware — reference existing M2 manual list)

**Acceptance Criteria:**
- [ ] README install/upgrade docs accurate per final behavior (no claims beyond implementation)
- [ ] Spec "M2 Completion Record" section added (mirror M1's): landed items, deviations, M2 manual-verification list referenced as still-pending user action

**Verify:** doc-only; `cargo check --workspace` still green.

Commit: `docs: M2 install/upgrade documentation and completion record`.

---

## Self-Review (plan-time)

- **Spec coverage (M2):** upgrade→Rust ✅(T2/T3) install.ps1 ✅(T4) lifecycle-native ✅(M1, recorded) windows artifacts ✅(M1) test-dist windows ✅(T5) docs ✅(T6). Manual verification stays with the user (hardware required) — recorded in T6.
- **Placeholder scan:** Task 2 explicitly resolves the stub-vs-complete tension (check-mode complete, full-mode error message pinned by test until T3 replaces it in the immediately-following task).
- **Type consistency:** `release_suffix`/`swap_binary_into`/`ReleaseInfo` names used consistently across tasks.
- **Known risk:** GitHub API rate-limit (unauthenticated 60/h) — same exposure as upgrade.sh today; no new mitigation in M2 (note in T2 doc comment).
