# Windows Sandbox No-Console Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent aHand's Windows restricted Python processes from allocating visible console windows, then pin Coffice to the fixed aHand revision.

**Architecture:** Centralize the flags passed to `CreateProcessAsUserW` in a small Windows-only helper and include `CREATE_NO_WINDOW` alongside the existing suspended-launch and extended-startup flags. Validate the exact flag set with a unit test, push aHand, then update Coffice's pinned Git revision to the resulting immutable commit.

**Tech Stack:** Rust, Win32 `CreateProcessAsUserW`, `windows-sys`, Cargo, Git

## Global Constraints

- Keep restricted-token launch, explicit stdio handle inheritance, job-object assignment, and timeout behavior unchanged.
- Do not replace `python.exe` with `pythonw.exe`.
- Do not change sandbox policy, installer behavior, or unrelated process-launch paths.
- Coffice must pin the exact 40-character aHand commit revision.

---

### Task 1: Suppress the restricted Windows child console

**Files:**
- Modify: `crates/ahandd/src/sandbox/platform/windows/process.rs`

**Interfaces:**
- Consumes: Win32 creation flag constants from `windows_sys::Win32::System::Threading`.
- Produces: `fn restricted_process_creation_flags() -> u32`, used by `CreateProcessAsUserW`.

- [ ] **Step 1: Write the failing flag regression test**

Add this Windows-only unit test to the existing `tests` module:

```rust
#[cfg(windows)]
#[test]
fn restricted_process_creation_flags_suppress_console_and_preserve_launch_contract() {
    let flags = restricted_process_creation_flags();

    assert_ne!(flags & CREATE_NO_WINDOW, 0);
    assert_ne!(flags & CREATE_SUSPENDED, 0);
    assert_ne!(flags & CREATE_UNICODE_ENVIRONMENT, 0);
    assert_ne!(flags & EXTENDED_STARTUPINFO_PRESENT, 0);
}
```

Import `CREATE_NO_WINDOW` with the existing Windows threading constants. Add
`restricted_process_creation_flags()` initially with the existing production
flags only, so the new assertion for `CREATE_NO_WINDOW` fails for the diagnosed
reason.

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```powershell
cargo test -p ahandd restricted_process_creation_flags_suppress_console_and_preserve_launch_contract
```

Expected: FAIL because `flags & CREATE_NO_WINDOW` equals zero.

- [ ] **Step 3: Implement the minimal production fix**

Change the helper to:

```rust
#[cfg(windows)]
fn restricted_process_creation_flags() -> u32 {
    CREATE_NO_WINDOW
        | CREATE_SUSPENDED
        | CREATE_UNICODE_ENVIRONMENT
        | EXTENDED_STARTUPINFO_PRESENT
}
```

Replace the inline `CreateProcessAsUserW` flag expression with
`restricted_process_creation_flags()`.

- [ ] **Step 4: Verify the aHand fix**

Run:

```powershell
cargo test -p ahandd restricted_process_creation_flags_suppress_console_and_preserve_launch_contract
cargo test -p ahandd sandbox::platform::windows::process
cargo fmt --all -- --check
cargo clippy -p ahandd --all-targets -- -D warnings
```

Expected: every command exits zero; the focused test and the existing 13
Windows process tests pass.

- [ ] **Step 5: Commit and push aHand**

```powershell
git add -- crates/ahandd/src/sandbox/platform/windows/process.rs
git commit -m "fix(windows): suppress restricted process consoles"
git fetch origin dev
git push origin HEAD:dev
```

Record the resulting 40-character commit SHA for Task 2.

### Task 2: Pin Coffice to the fixed aHand revision

**Files:**
- Modify: `apps/desktop/src-tauri/Cargo.toml`
- Modify: `apps/desktop/src-tauri/Cargo.lock`

**Interfaces:**
- Consumes: the 40-character aHand commit SHA produced by Task 1.
- Produces: a reproducible Coffice desktop dependency pin containing the no-console fix.

- [ ] **Step 1: Create a clean Coffice worktree from the latest `origin/dev`**

Fetch Coffice `origin/dev`, verify the local worktree directory is ignored,
then create branch `codex/windows-sandbox-no-console` in an isolated worktree.

- [ ] **Step 2: Write the failing dependency-pin regression**

In `scripts/validate-desktop-rust-release-config.test.mjs`, add an assertion
that the `ahandd` dependency revision equals the exact Task 1 commit:

```js
assert.match(
  dependency,
  new RegExp(`\\brev\\s*=\\s*"${fixedAhandRevision}"`),
);
```

Define `fixedAhandRevision` as the literal 40-character Task 1 SHA.

- [ ] **Step 3: Run the release configuration test and verify RED**

Run:

```powershell
node --test scripts/validate-desktop-rust-release-config.test.mjs
```

Expected: FAIL because `Cargo.toml` still pins the previous aHand revision.

- [ ] **Step 4: Update the dependency and lockfile**

Replace the `ahandd` `rev` in `apps/desktop/src-tauri/Cargo.toml` with the Task
1 SHA, then run:

```powershell
$fixedAhandRevision = git -C 'D:\Projects\weightwave\aHand\.worktrees\windows-sandbox-no-console' rev-parse HEAD
cargo update --manifest-path apps/desktop/src-tauri/Cargo.toml -p ahandd --precise $fixedAhandRevision
```

Confirm the `ahandd`, `ahand-platform`, and `ahand-protocol` lockfile source
entries all resolve to the same new Git commit.

- [ ] **Step 5: Verify Coffice**

Run:

```powershell
node --test scripts/validate-desktop-rust-release-config.test.mjs
cargo check --manifest-path apps/desktop/src-tauri/Cargo.toml
cargo fmt --manifest-path apps/desktop/src-tauri/Cargo.toml -- --check
git diff --check
```

Expected: every command exits zero.

- [ ] **Step 6: Commit and push Coffice**

```powershell
git add -- apps/desktop/src-tauri/Cargo.toml apps/desktop/src-tauri/Cargo.lock scripts/validate-desktop-rust-release-config.test.mjs
git commit -m "fix(windows): update aHand for hidden sandbox consoles"
git push origin HEAD:dev
```

Verify with `git ls-remote origin refs/heads/dev` that remote `dev` points to
the new Coffice commit.
