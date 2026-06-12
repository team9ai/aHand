# M4 — Windows Security & Test Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the M4 security backlog so Windows reaches parity-where-possible with the Unix security model: native symlink creation, ACL-based chmod, hardened `sanitize_env`, case-correct policy matching, an explicit named-pipe security descriptor, and Windows variants of the file-ops security regression suite — so the daemon's security tests run (and pass) on the Windows CI lane.

**Architecture:** Targeted hardening in existing modules; no new crates beyond `windows-sys` (already pulled transitively — verify) for the pipe SD and ACL work. Each fix is independently testable. Spec backlog: `docs/superpowers/specs/2026-06-11-cross-platform-windows-linux-design.md` § M1 Completion Record → M4 list.

**Honest scope boundary (do NOT over-promise):** Windows has **no openat2/O_NOFOLLOW equivalent** — the TOCTOU race that `io_safe` closes on Unix CANNOT be fully closed on Windows. M4 does NOT claim TOCTOU parity. What M4 DOES: (a) post-open handle re-verification (`GetFinalPathNameByHandle` → re-check against policy) to catch naive junction/symlink misdirection, documented as narrowing-not-closing the window; (b) everything else (symlink create, ACL chmod, env, policy case, pipe SD, tests) to real parity. The single-tenant-host assumption stays documented for the residual TOCTOU window.

**Dev-loop:** macOS dev box. ahandd Windows code can't run locally (lzma-sys); keep `#[cfg(windows)]` review-clean, lean on the Windows CI lane (client-ci `test (windows-latest)`) as the gate. Where a Windows API path is added, also `cargo check --target x86_64-pc-windows-msvc -p ahand-platform` for the platform crate.

---

## File Structure

```
Modify:
  crates/ahandd/src/file_manager/fs_ops.rs          # symlink create (T1), ACL chmod (T2)
  crates/ahandd/src/file_manager/policy.rs          # case-insensitive match on windows (T4)
  crates/ahandd/src/openclaw/handler.rs             # sanitize_env hardening (T3), shell e2e tests cmd/C (T5)
  crates/ahand-platform/src/ipc.rs                  # explicit pipe security descriptor (T6)
  crates/ahand-platform/Cargo.toml                  # + windows-sys feature(s) if needed (T6)
  proto/ (file_ops)                                 # ONLY if WindowsAcl message lacks fields T2 needs
  crates/ahandd/tests/file_ops.rs / file_ops_e2e.rs # windows security regression variants (T7)
Docs: spec M4 Completion Record.
```

Task order: 1, 3, 4, 6 are independent (any order); 2 may need a proto read first; 5 depends on nothing; 7 last (exercises 1/2/4). Group reviews: {1,2} file-ops, {3,5} openclaw, {4} policy, {6} ipc, {7} tests.

---

### Task 1: Native symlink creation on Windows

**Goal:** `handle_create_symlink` creates real symlinks on Windows instead of returning "not supported".

**Files:** `crates/ahandd/src/file_manager/fs_ops.rs` (handle_create_symlink ~996-1042)

**Acceptance Criteria:**
- [ ] Windows arm uses `std::os::windows::fs::{symlink_file, symlink_dir}` — choose by inspecting the TARGET: if the (policy-resolved) target exists and is a directory → `symlink_dir`, else `symlink_file` (document: a dangling target defaults to file-symlink, matching common tooling)
- [ ] Privilege/developer-mode missing → map the OS error (ERROR_PRIVILEGE_NOT_HELD = 1314) to a CLEAR `FileError` with remediation text ("enable Developer Mode or run elevated"), not a generic Io error
- [ ] The link path still goes through policy (`link_resolved` is the post-check path — confirm the caller validates it); the target must ALSO be policy-checked the same way the Unix path does (read how Unix validates target — R2 "symlink target outside allowlist denied at preflight"; preserve that on Windows)
- [ ] Unix arm byte-identical; the load-bearing `return` cfg pattern preserved
- [ ] Tests: `#[cfg(windows)]` create-file-symlink + create-dir-symlink happy paths (tempdir); target-outside-allowlist denied (parity with Unix R2 test); privilege-error mapping unit-tested via the error-translation helper (pure fn over a raw errno)

**Verify:** `cargo test -p ahandd file_manager && cargo clippy -p ahandd --all-targets -- -D warnings && cargo fmt -p ahandd -- --check && cargo check --workspace`

Commit: `feat(file-ops): native Windows symlink creation with privilege-error mapping`

---

### Task 2: Windows ACL chmod

**Goal:** `handle_chmod` `Permission::Windows(acl)` arm actually sets ACLs on Windows.

**Files:** `crates/ahandd/src/file_manager/fs_ops.rs` (handle_chmod ~1046-1112); READ the proto `WindowsAcl`/`file_chmod::Permission::Windows` message FIRST (proto/ file_ops) to learn the exact wire surface.

**Acceptance Criteria:**
- [ ] READ the `Permission::Windows` proto payload; the implementation honors EXACTLY what the proto exposes (don't invent fields). If the proto's Windows ACL surface is too thin to be meaningful (e.g. only a placeholder), STOP and report to the controller with the proto contents + a recommendation (likely: a minimal "restrict to owner / grant principal:rights" surface) rather than guessing — this is a plan-gate.
- [ ] Implementation via `icacls` subprocess (consistent with M1's `secure_file::restrict_to_owner` precedent — reuse that style) OR the `windows-sys` SetNamedSecurityInfo API; PREFER icacls for parity with existing code unless the proto demands fine-grained ACEs. Document the choice.
- [ ] Non-windows arm unchanged ("Windows ACLs are not supported on this platform")
- [ ] Symlink-leaf rejection (`reject_if_final_component_is_symlink`) still runs before the ACL op (it already does — preserve)
- [ ] Tests: `#[cfg(windows)]` — set an ACL on a tempfile, read it back via `icacls` and assert the expected principal/rights; invalid principal → clear error. Unit-test any pure ACL-string builder.

**Verify:** `cargo test -p ahandd file_manager && cargo clippy -p ahandd --all-targets -- -D warnings && cargo fmt -p ahandd -- --check`

Commit: `feat(file-ops): Windows ACL chmod`

---

### Task 3: Harden `sanitize_env` for Windows

**Goal:** `sanitize_env` validates PATH overrides platform-correctly and blocks Windows injection vectors.

**Files:** `crates/ahandd/src/openclaw/handler.rs` (sanitize_env ~1334-1382)

**Acceptance Criteria:**
- [ ] PATH-override validation uses `std::env::split_paths`/`join_paths` semantics instead of the `format!(":{}", base_path)` literal — the "only allow if it prepends to current PATH" rule must hold on Windows (`;` separator). Concretely: split both override and base PATH; accept iff base's entries are a contiguous suffix of the override's entries (the "prepend-only" contract). Add a helper `path_override_is_prepend_only(override_val, base) -> bool` and unit-test it cross-platform (feed `;`-joined values under cfg(windows), `:`-joined under cfg(unix)).
- [ ] BLOCKED_PREFIXES / BLOCKED_KEYS gain Windows loader/injection vars: at minimum `__PSLockdownPolicy`-class is N/A, but DO add the cross-platform-relevant ones that apply on Windows — research the real set: Windows DLL-search hijack vectors don't use env the way DYLD_/LD_ do, BUT `NODE_OPTIONS`/`PYTHONPATH`/`PYTHONHOME` (already blocked) are the high-value ones for this daemon (it spawns node/python). Confirm those stay blocked on all platforms (they're key-exact, already platform-neutral). Add any genuinely Windows-specific high-risk var you can justify (e.g. `PYTHONEXECUTABLE`); do NOT pad the list with speculative entries (per the "verify risks are actually risks" standard — each addition must name a concrete injection mechanism in a comment).
- [ ] Behavior on Unix unchanged for the existing blocked set
- [ ] Tests: `path_override_is_prepend_only` unit tests (prepend accepted, replace/insert-middle rejected, empty handled) — cfg-gated separators; blocked-key tests stay green

**Verify:** `cargo test -p ahandd openclaw && cargo clippy -p ahandd --all-targets -- -D warnings && cargo fmt -p ahandd -- --check`

Commit: `fix(openclaw): platform-correct PATH-override validation and env blocklist`

---

### Task 4: Case-correct policy matching on Windows

**Goal:** `glob_match`/`check_path` matches case-insensitively on Windows (NTFS semantics) without weakening Unix.

**Files:** `crates/ahandd/src/file_manager/policy.rs` (glob_match ~265, check_path matching ~93-197)

**Acceptance Criteria:**
- [ ] On Windows, glob matching is case-insensitive: lowercase BOTH the pattern and the resolved path before `glob::Pattern` matching (or use the glob crate's case-insensitive option if it exists — check `MatchOptions`; `glob::Pattern::matches_with(.., MatchOptions{case_sensitive:false,..})` is the clean route). Unix stays case-sensitive (unchanged).
- [ ] Separator consistency: `canonicalize_simplified` (M1) already strips `\\?\`; confirm the resolved string uses backslashes on Windows and patterns from config (also expanded via `home.join`) likewise — if a forward-slash vs backslash mismatch exists, normalize one way before matching and document. DECIDE by reading: config patterns like `~/.ssh/**` expand via `home.join("...")` → backslashes on Windows; canonicalized paths → backslashes; glob `**` works across both `/` and `\` in the glob crate (verify — the glob crate treats `\` as escape on Unix but as separator on Windows? CHECK and document the actual behavior; if `\` is an escape char, normalize both sides to `/` before matching).
- [ ] This is the spec's "Windows glob case-insensitivity hardening" item; also re-validates the M1 fail-closed UNC/long-path note still holds
- [ ] Tests: `#[cfg(windows)]` — `C:\Users\X\**` matches `c:\users\x\file` (case); allowlist with mixed case admits the canonical-cased resolved path; a denylist entry matches case-insensitively (security-relevant: `~/.ssh` must block `~/.SSH`). Unix case-sensitivity regression test (pattern `/Home` does NOT match `/home`).

**Verify:** `cargo test -p ahandd file_manager && cargo clippy -p ahandd --all-targets -- -D warnings && cargo fmt -p ahandd -- --check`

Commit: `fix(file-policy): case-insensitive matching on Windows (NTFS parity)`

---

### Task 5: Port the two strict-mode shell e2e tests to run on Windows

**Goal:** `strict_mode_*` openclaw e2e tests run on Windows (using `cmd /C` + a Windows-writable temp path), proving shell-job execution + approval flow on Windows.

**Files:** `crates/ahandd/src/openclaw/handler.rs` (the two `#[cfg(unix)]` tests ~1485-1555, and `unique_output_path`)

**Acceptance Criteria:**
- [ ] `unique_output_path()` uses `std::env::temp_dir()` (cross-platform) instead of a hardcoded `/tmp`
- [ ] The two tests build a platform-appropriate command: unix `printf X > path`; windows `cmd /C echo X> path` (mind `cmd` redirection quoting — verify the output assertion accounts for any trailing space/newline `echo` adds; read back and `.trim()` or assert `.contains`)
- [ ] Remove the `#[cfg(unix)]` gate so they run on both; if the command/shell resolution path differs, route through the existing executor shell logic (the daemon's own shell resolution from M1 — these tests drive `system_run_invoke`, which goes through the real exec path; ensure that path picks COMSPEC on Windows)
- [ ] Both tests assert the same invariants (pre-approved skips broadcast; strict waits for approval) on both platforms
- [ ] On Windows CI they actually execute a real `cmd /C` job end-to-end (this is the spec's "un-gate shell-exec e2e" item)

**Verify:** `cargo test -p ahandd openclaw && cargo clippy -p ahandd --all-targets -- -D warnings && cargo fmt -p ahandd -- --check`

Commit: `test(openclaw): run strict-mode shell e2e on Windows (cmd /C, temp_dir)`

---

### Task 6: Explicit named-pipe security descriptor

**Goal:** The Windows IPC pipe is created with an EXPLICIT owner-only DACL instead of relying on tokio/OS defaults — closing the Codex-flagged "default SD grants Everyone READ" gap.

**Files:** `crates/ahand-platform/src/ipc.rs` (windows bind/accept ~130-216), `crates/ahand-platform/Cargo.toml`

**Acceptance Criteria:**
- [ ] All THREE `ServerOptions::new().create()` call sites (first-instance, lazy-recreate, pre-create-next) go through ONE helper `create_secured_pipe(name, first_instance) -> Result<NamedPipeServer>` (kills the current triplication) that sets an explicit security descriptor: DACL granting full access to the creating user SID + SYSTEM + Administrators only, denying Everyone. Use `tokio::net::windows::named_pipe::ServerOptions::create_with_security_attributes_raw` with a SECURITY_ATTRIBUTES built from a SDDL string via `windows-sys` `ConvertStringSecurityDescriptorToSecurityDescriptorW` (SDDL like `D:P(A;;GA;;;OW)(A;;GA;;;SY)(A;;GA;;;BA)` — owner/system/builtin-admins full, no Everyone ACE). Verify the exact SDDL + windows-sys feature flags needed; add the minimal `windows-sys` features to Cargo.toml.
- [ ] Module doc updated: the per-user DACL is now ENFORCED in code, not assumed from defaults (correct the M1/M3 wording)
- [ ] `mode` arg semantics unchanged (still unix-only); the helper is windows-only
- [ ] Memory safety: the SDDL→SD conversion allocates; ensure the SD lives for the duration of the create call and is freed (LocalFree) after — RAII guard or careful scope; no leak, no use-after-free. This is unsafe FFI — keep the unsafe block minimal and commented.
- [ ] Tests: the existing loopback/sequential pipe tests still pass (they prove the secured pipe still accepts the OWNER); a full cross-user rejection test is NOT CI-achievable (single principal on the runner) — document that the cross-user-rejection verification stays on the M2 manual-list. If a pure SDDL-builder fn is extracted, unit-test the string it produces.

**Verify:** `cargo test -p ahand-platform ipc && cargo check --target x86_64-pc-windows-msvc -p ahand-platform && cargo clippy -p ahand-platform --all-targets --target x86_64-pc-windows-msvc -- -D warnings && cargo clippy -p ahand-platform --all-targets -- -D warnings && cargo fmt -p ahand-platform -- --check`

Commit: `feat(ipc): explicit owner-only security descriptor on Windows named pipe`

NOTE: this is the highest-FFI-risk task. If `create_with_security_attributes_raw` + windows-sys SDDL proves to need more than ~80 lines of unsafe glue, report DONE_WITH_CONCERNS with the working code + a note, rather than over-engineering. The CI windows lane only proves it compiles+the owner can still connect; the DACL correctness is verified by the unit-tested SDDL string + M2 manual cross-user test.

---

### Task 7: Windows variants of the file-ops security regression suite

**Goal:** The `#[cfg(unix)]` security regression tests get Windows analogues where an analogue exists, so the Windows CI lane exercises the security model (not zero coverage).

**Files:** `crates/ahandd/tests/file_ops.rs`, `crates/ahandd/tests/file_ops_e2e.rs`

**Acceptance Criteria:**
- [ ] Inventory the `#[cfg(unix)]` tests (the extraction has the list). For EACH, classify: (a) has-windows-analogue → write it; (b) no-analogue (openat2 TOCTOU) → leave unix-gated with a `// no Windows equivalent: openat2 TOCTOU — single-tenant assumption (spec M4)` comment.
- [ ] Windows analogues to write: symlink-escape denial (now that T1 creates symlinks — a symlink/junction pointing outside the allowlist must be DENIED by policy on Windows too); chmod/ACL assertions (T2 — set+read-back); case-insensitive denylist block (T4); the no_follow_symlink lstat-not-metadata behavior (cross-platform — verify it's tested on windows). Junctions: create via `std::os::windows::fs::symlink_dir` or a junction helper and assert escape is denied.
- [ ] ALL test fixtures route canonicalization through `ahand_platform::paths::canonicalize_simplified` (NOT raw `.canonicalize()`) — the M1 landmine; grep the touched tests to confirm.
- [ ] The Windows CI `test (windows-latest)` lane runs these and is green
- [ ] No reduction in unix coverage (existing unix tests untouched except where genuinely shared)

**Verify:** `cargo test -p ahandd --test file_ops --test file_ops_e2e && cargo clippy -p ahandd --all-targets -- -D warnings && cargo fmt -p ahandd -- --check` (windows-specific tests verified on CI)

Commit: `test(file-ops): Windows security regression suite (symlink/junction escape, ACL, case)`

---

## Self-Review (plan-time)

- **Spec M4 coverage:** symlink ✅(T1) ACL chmod ✅(T2) sanitize_env ✅(T3) glob case ✅(T4) shell e2e un-gate ✅(T5) pipe SD ✅(T6) security-suite windows variants ✅(T7). Every M4-backlog line maps to a task.
- **Honest non-goals:** openat2 TOCTOU parity is explicitly NOT claimed (documented in T7 + the scope boundary); cross-user pipe rejection verification stays on the M2 manual list (T6).
- **Proto gate:** T2 reads the WindowsAcl proto first and escalates if the surface is inadequate rather than inventing — this is the one task that could need a proto change (additive, would trigger `cargo check --workspace` per the shared-crate memory).
- **Highest risk:** T6 unsafe FFI — bounded with a DONE_WITH_CONCERNS escape and a unit-tested SDDL string as the correctness anchor.
- **Placeholder scan:** none; READ-first instructions where Windows API/proto/glob behavior must be confirmed empirically.
