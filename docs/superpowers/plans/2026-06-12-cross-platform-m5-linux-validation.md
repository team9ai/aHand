# M5 — Linux Validation & Cross-Platform Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the M5 backlog so Linux is a first-class, validated platform and the user-facing surfaces (CLI remediation, admin panel, install docs, install script security) are platform-correct on all three OSes — reaching the M5 exit criterion: *Linux e2e green (and non-flaky) in CI; docs cover all three platforms.*

**Reframe (grounded by the M5 understand pass):** Most of the spec's M5 framing was ALREADY satisfied before this milestone — the BATS e2e suite already runs on `ubuntu-latest` (`.github/workflows/test-dist-scripts.yml` matrix, since commit `4266200` Feb-2026), the README already documents all three platforms with both one-liners, and every config/data path already resolves cross-platform via `dirs::home_dir()` (no macOS `~/Library` anywhere). So M5 is NOT "add the Linux lane" — it is closing the concrete residuals the understand pass found.

**Architecture:** Targeted fixes in existing modules + shell scripts + the admin SPA. No new crates. Each task is independently testable; the BATS-affecting tasks are verified under FORCED parallel mode (`bats --jobs`) because that is the path the Linux CI lane actually runs and macOS never does.

**Honest scope boundary (do NOT over-promise / do NOT over-build):**
- **musl / Alpine / armv7 are explicitly OUT of M5 scope** (spec § Out of scope, lines 211-212). Released Linux binaries stay glibc x86_64/aarch64. Document the boundary; do not chase a musl target.
- The "Linux e2e green" gate is only meaningful if the suite is non-flaky under `--jobs`. A single green ubuntu run is NOT sufficient proof — the `/tmp` race fix (T1) must be verified under forced parallel mode locally (install GNU `parallel`) AND on CI.
- `/api/status` extension (T5) stays ADDITIVE — `StatusResponse` is a serialized contract shared by `crates/ahandctl/src/admin.rs` and `apps/admin/src/lib/api.ts`; keep both in sync and run `cargo check --workspace`.

**Dev-loop:** macOS dev box; the BATS suite runs locally (46/46 today). To exercise the Linux-only parallel path, `brew install parallel` and run `bats --jobs 3 e2e/scripts/*.bats` (or force `parallel` onto PATH for `run.sh`). The Rust + admin-SPA changes build/test locally. Lean on `test-dist-scripts` (ubuntu-latest) + `test (ubuntu-latest)` CI lanes as the final gate.

---

## File Structure

```
Modify:
  scripts/dist/install.sh                                  # /tmp race fix (T1), SHA-256 verify (T3)
  e2e/scripts/run.sh                                       # GNU-parallel-aware --jobs gating (T2)
  e2e/scripts/install.bats                                 # de-reference shared /tmp path (T1); checksum tests (T3)
  e2e/scripts/mocks/curl                                   # serve .sha256 for checksum tests (T3)
  crates/ahandd/src/cli/browser_doctor.rs                  # host-OS remediation filter (T4)
  crates/ahandctl/src/admin.rs                             # StatusResponse home_dir/bin_dir (T5)
  apps/admin/src/lib/api.ts                                # StatusResponse TS mirror (T5)
  apps/admin/src/panels/SetupPanel.tsx                     # real config_path from /api/status (T5)
  apps/admin/src/panels/ConfigPanel.tsx                    # data_dir/bin placeholders from /api/status (T5)
  README.md                                                # per-platform install docs + true checksum-parity claim (T6)
```

---

## Task 1 — Fix the shared `/tmp/ahand-admin-spa.tar.gz` race (Linux parallel-safety)

`scripts/dist/install.sh:114-116` downloads/extracts/removes a hardcoded global path. ~16 `install.bats` tests run install.sh fully; under bats `--jobs` (Linux CI only) they collide → `install.bats:121`'s `[ ! -f /tmp/ahand-admin-spa.tar.gz ]` flakes. macOS never hits this (sequential).

**Files:** `scripts/dist/install.sh`, `e2e/scripts/install.bats`

**Acceptance Criteria:**
- [ ] install.sh uses a per-invocation temp (e.g. `$(mktemp -d)/ahand-admin-spa.tar.gz` or `${TMPDIR:-/tmp}/ahand-admin-spa.$$.tar.gz`) with `trap '... rm -rf' EXIT` cleanup, mirroring `upgrade.sh:124-125`.
- [ ] `install.bats:121` no longer asserts on the fixed global path; it asserts the install succeeded and leaves no leftover temp in `TEST_HOME` (or the isolated temp dir).
- [ ] No behavior change in the non-test (real) install path — the SPA still lands where it does today.

**Verify:** `brew install parallel` then `cd e2e/scripts && bats --jobs 3 install.bats` green **10× in a row** (prove the race is gone); full `bash e2e/scripts/run.sh` green.

---

## Task 2 — GNU-parallel-aware `--jobs` gating in run.sh

`e2e/scripts/run.sh:12-13` sets `--jobs 3` on bare `command -v parallel`. A moreutils `parallel` makes bats-core abort hard (`Cannot execute jobs without GNU parallel`). ubuntu-latest ships GNU parallel so the hosted lane survives, but the detection is fragile.

**Files:** `e2e/scripts/run.sh`

**Acceptance Criteria:**
- [ ] `--jobs N` is enabled only when the `parallel` on PATH is GNU parallel (`parallel --version 2>/dev/null | grep -q 'GNU parallel'`); otherwise fall back to sequential (no abort).
- [ ] A clear `echo` notes which mode was chosen (parallel vs sequential) for CI log legibility.
- [ ] No regression on the hosted ubuntu-latest lane (still parallel there).

**Verify:** On a host with only moreutils `parallel` on PATH (simulate by shimming a fake `parallel` that prints a non-GNU version), run.sh runs sequentially without aborting; on a host with GNU parallel it runs `--jobs`.

---

## Task 3 — SHA-256 verification in install.sh (true cross-platform parity)

`install.ps1:111-141` verifies SHA-256 of downloaded artifacts; `install.sh` does NOT — yet `README.md:60` claims parity. Release CI already publishes `.sha256` checksums (`release-rust.yml:86-88`). **User-approved: add verification to install.sh** (do not weaken the doc).

**Files:** `scripts/dist/install.sh`, `e2e/scripts/install.bats`, `e2e/scripts/mocks/curl`

**Acceptance Criteria:**
- [ ] install.sh downloads the artifact's `.sha256` (same URL pattern as install.ps1), computes the local digest with `shasum -a 256` (macOS) / `sha256sum` (Linux) — detect which is available, mirroring the existing `mocks/shasum` fallback — and ABORTS with a clear error if they mismatch. Fail-closed: a missing/unreadable checksum is an error, not a skip.
- [ ] The check covers the SAME artifacts install.ps1 verifies (the release tarball/binary). Match install.ps1's scope; do not invent new verification surface.
- [ ] `mocks/curl` serves a correct `.sha256` for the fixture so the happy-path bats tests pass.
- [ ] NEW bats tests: (a) happy path — matching checksum → install succeeds; (b) **tampered artifact** — checksum mismatch → install FAILS with a non-zero exit and a clear message (fail-closed regression test); (c) missing `.sha256` → fail-closed.
- [ ] Non-test real install verifies against the real published checksum.

**Verify:** `cd e2e/scripts && bats install.bats` green incl. the 3 new tests; `bats --jobs 3 install.bats` green (interacts with T1).

---

## Task 4 — Filter browser-doctor remediation to the host platform

`crates/ahandd/src/cli/browser_doctor.rs:105-110` prints ALL platforms' install commands, so a Linux user sees `brew install --cask google-chrome`. The per-platform data (`browser_setup/mod.rs:234-248`) is correct; only the CLI renderer is unfiltered.

**Files:** `crates/ahandd/src/cli/browser_doctor.rs`, `crates/ahandd/src/browser_setup/types.rs` (helper only if needed)

**Acceptance Criteria:**
- [ ] The human-facing `print_fix_hint` prints ONLY the host platform's command, mapping the host to the display label (`"macOS"`/`"Linux"`/`"Windows"`, the vocabulary at `types.rs:72`) via a `cfg!(target_os=...)` helper — NOT `std::env::consts::OS` (which is `"macos"` not `"macOS"` and would match nothing).
- [ ] Fall back to printing all entries only if the host label has no match (unknown OS) — never silently print nothing.
- [ ] The serialized/JSON form (`#[derive(Serialize)]` `platform_commands`, consumed by `/api/status`/admin) is UNCHANGED — keep all platforms in the data; filter only in the println path.
- [ ] Unit test: on the host target, exactly one command line is printed and it matches the host label (e.g. on Linux output contains the apt/chromium command and does NOT contain `brew install`).
- [ ] Respect the `TODO(task-2): proper failure rendering` note at `browser_doctor.rs:88` — keep the change minimal/forward-compatible.

**Verify:** `cargo test -p ahandd browser_doctor && cargo check --workspace && cargo clippy -p ahandd --all-targets -- -D warnings && cargo fmt -p ahandd -- --check`

---

## Task 5 — Resolve admin-panel placeholder paths from `/api/status`

Three hardcoded `~/.ahand/...` literals mislead non-macOS users (wrong on Windows; tilde not absolute). The backend already resolves paths cross-platform; the SPA just needs to read them.

**Files:** `crates/ahandctl/src/admin.rs`, `apps/admin/src/lib/api.ts`, `apps/admin/src/panels/SetupPanel.tsx`, `apps/admin/src/panels/ConfigPanel.tsx`

**Acceptance Criteria:**
- [ ] **(5a, backend)** Extend `StatusResponse` (`admin.rs:14-22`) with `home_dir` and `bin_dir`, populated in `get_status` from `dirs::home_dir()` / `home.join(".ahand").join("bin")`, mirroring `get_data_dir` (`admin.rs:682-685`). Additive only. Mirror the fields in the TS `StatusResponse` (`api.ts:76-83`). `cargo check --workspace` clean (shared serialized shape).
- [ ] **(5b, SetupPanel)** The post-save confirmation (`SetupPanel.tsx:72-84`) renders the real `status.config_path` from `/api/status` instead of the literal `~/.ahand/config.toml`; falls back to the literal only if the status call fails (status works pre-daemon — paths come from `dirs::home_dir()`).
- [ ] **(5c, ConfigPanel)** Data Directory placeholder (`ConfigPanel.tsx:363`) uses `status.data_dir`; Browser Binary placeholder (`ConfigPanel.tsx:497`) uses `status.bin_dir + "/agent-browser"` (platform-correct separator handled by backend-supplied absolute path). Degrade gracefully to the current literals if the call fails.
- [ ] No `~/`-prefixed literal remains as a *displayed* path in these three sites (placeholders now absolute, host-correct).

**Verify:** `cargo test -p ahandctl` (admin module) + `cargo check --workspace`; build the admin SPA (`pnpm --filter @ahand/admin build` or the repo's admin build lane); confirm TS `StatusResponse` matches the Rust struct. If the admin SPA has a test lane, assert no `~/` literal renders.

---

## Task 6 — Per-platform install docs + true checksum-parity claim

README's install section covers all three platforms at one-liner granularity but lacks per-platform notes, and `README.md:60` claims checksum parity that only becomes true after T3.

**Files:** `README.md`

**Acceptance Criteria:**
- [ ] Install section split into three per-platform subsections (macOS / Linux / Windows), each with its one-liner + post-install notes: macOS (Gatekeeper auto-handled, manual PATH step), Linux (glibc build, no quarantine step, manual PATH step), Windows (PATH auto-updated by install.ps1 — open a new terminal; Win10 1809+).
- [ ] After-install PATH guidance documented (macOS/Linux print a profile line; Windows auto-updates user PATH).
- [ ] The checksum-parity claim (`README.md:60`) is now TRUE for all three (depends on T3) — state that all installers verify SHA-256.
- [ ] One-sentence Linux-distro caveat: released Linux binaries are glibc x86_64/aarch64 (`release-rust.yml:22-26`); musl/Alpine and armv7 are not provided. (Documents the boundary; does NOT propose musl work.)
- [ ] Log-path note generalized to resolve under `%USERPROFILE%` on Windows (`daemon.rs:5-16`).

**Verify:** Doc review against the cited script/source lines; `npx markdown-link-check README.md` (or equivalent) for the install URLs.

---

## Task 7 — Verify Linux e2e is green AND non-flaky (M5 exit gate)

**Files:** none (verification) — may touch `.github/workflows/test-dist-scripts.yml` only if a gate/required-check change is needed.

**Acceptance Criteria:**
- [ ] After T1–T3 land, the BATS suite is green under FORCED parallel mode locally (`bats --jobs 3 e2e/scripts/*.bats`, ≥10 iterations) — proving the `/tmp` race is gone, not just luckily green.
- [ ] The `test-dist-scripts` ubuntu-latest job is green on the M5 PR (real CI run).
- [ ] The `test (ubuntu-latest)` Rust lane is green (T4/T5 Rust changes).
- [ ] Record the green run IDs in the spec's M5 Completion Record.

**Verify:** `gh run list --workflow=test-dist-scripts.yml` shows a green ubuntu-latest run on the PR head; local 10× parallel pass logged.

---

## Review & Finish

Per the established M1–M4 pattern: after implementation, run independent multi-round review (code-quality + spec-conformance reviewers, plus Codex/Copilot on the PR), a test-completeness audit, and a doc-consistency audit; fix all Critical/Important findings; then finish-feature → PR to `main` (user confirms merge timing) → main→dev sync.

**Spec Completion Record:** append an M5 Completion Record to `docs/superpowers/specs/2026-06-11-cross-platform-windows-linux-design.md` documenting what landed, the reframe (most of M5 was pre-satisfied), and the residuals closed.


