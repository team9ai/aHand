# Windows Sandbox Handoff

Date: 2026-06-25

## Repository State

- Repository: `team9ai/aHand`
- Worktree used on macOS: `/Users/winrey/Projects/weightwave/aHand/.worktrees/windows-coffice-sandbox`
- Branch: `codex/windows-coffice-sandbox`
- Base: `origin/dev`
- Latest implementation commit before these handoff docs: `081cf6e feat(sandbox): wire Windows sandbox readiness`
- Current branch is intended for Windows sandbox validation and completion before merge.

## Goal

Finish the Windows implementation for aHand sandbox execution while preserving the platform-neutral API consumed by Coffice and aHand callers. The upper layer should continue to call the same sandbox APIs with `RuntimeSandboxPolicy { writable_root, readonly_roots, network }`; Windows-specific local user, firewall, ACL, and runner work stays behind `crates/ahandd/src/sandbox/platform/windows`.

## Design Rules

- Reference implementation: `~/Projects/github/codex/codex-rs/windows-sandbox-rs`.
- Network semantics must match macOS:
  - `NetworkPolicy::Enabled`: real network allowed.
  - `NetworkPolicy::Disabled`: strict hard block.
  - `NetworkPolicy::ProxyOnly`: unsupported for now.
- Do not enable Windows process execution until Windows live verification passes.
- Do not flip `process_execution_enabled()` by itself.
- Do not reintroduce direct current-process-token launch as the enabled path.
- The real Windows execution model must use a sandbox local user plus runner/logon flow, aligned with Codex `CreateProcessWithLogonW` + command runner.
- Filesystem roots must be derived through `DerivedFilesystemRoots`; do not apply ACLs to raw policy roots.
- macOS behavior must not regress.

## Completed On macOS

The branch contains these committed slices after `origin/dev`:

- `7e428e7 feat(sandbox): add shared runner preflight for proxy-only`
- `983f4c2 feat(sandbox): scaffold Windows backend helpers`
- `097f710 fix(sandbox): harden Windows helper scaffolding`
- `94d61b0 feat(sandbox): add Windows token and ACL primitives`
- `488be89 fix(sandbox): avoid ACL side effects in Windows stub`
- `a2e4766 fix(sandbox): plumb Windows network context`
- `4168ccc feat(sandbox): add Windows setup identity state primitives`
- `e5d2bc7 fix(sandbox): keep Windows disabled network fail-closed`
- `1de4838 feat(sandbox): gate Windows offline network on firewall readiness`
- `dcc7f9b fix(sandbox): verify Windows firewall readiness live`
- `9b4bff6 feat(sandbox): scaffold Windows process capture`
- `f3923e6 feat(sandbox): derive Windows filesystem roots`
- `081cf6e feat(sandbox): wire Windows sandbox readiness`

Current key behavior:

- Windows capture fails closed before setup/capability side effects while `process_execution_enabled()` is false.
- Online setup readiness loads online sandbox creds from existing state and fails closed when missing.
- Offline setup readiness requires verified hard network block.
- `DerivedFilesystemRoots` canonicalizes/dedupes roots and filters sandbox state/secrets.
- ACL planning grants prepared roots to both the sandbox users group and the per-workspace capability SID.
- Null DACL is not treated as prepared access.
- Even if `process_execution_enabled()` is accidentally changed to true, capture currently stops at `resolve_sandbox_user_runner_launch()` because runner/logon integration is intentionally unavailable.

## Important Files

- `crates/ahandd/src/sandbox/platform/windows/capture.rs`
  - `process_execution_enabled()` remains false.
  - `resolve_sandbox_user_runner_launch()` is the current fail-closed placeholder for the Codex-style logon runner.
- `crates/ahandd/src/sandbox/platform/windows/setup.rs`
  - `prepare_network_context()`
  - `run_online_setup()`
  - `run_offline_setup()`
- `crates/ahandd/src/sandbox/platform/windows/identity.rs`
  - `load_sandbox_creds_for_identity()`
  - `sandbox_setup_is_complete_for_identity()`
- `crates/ahandd/src/sandbox/platform/windows/sandbox_users.rs`
  - local users/group provisioning and SID resolution.
- `crates/ahandd/src/sandbox/platform/windows/firewall.rs`
  - Disabled-network firewall setup/verification.
- `crates/ahandd/src/sandbox/platform/windows/roots.rs`
  - `DerivedFilesystemRoots`
  - `derive_filesystem_roots()`
- `crates/ahandd/src/sandbox/platform/windows/acl.rs`
  - `apply_filesystem_roots()`
  - `allow_null_device()`
- `crates/ahandd/src/sandbox/platform/windows/process.rs`
  - dormant process capture scaffold; do not enable directly as the final launch model.
- `crates/ahandd/src/sandbox/platform/windows/token.rs`
  - dormant current-token restriction scaffold; useful reference, not the final enabled entrypoint.

## Codex Reference Map

Read these before changing runner/setup semantics:

- `~/Projects/github/codex/codex-rs/windows-sandbox-rs/src/setup_orchestrator.rs`
  - payload building, read/write root derivation, sensitive root filtering.
- `~/Projects/github/codex/codex-rs/windows-sandbox-rs/src/setup_main_win.rs`
  - elevated setup, read ACLs, write ACLs, sandbox dirs/secrets hardening.
- `~/Projects/github/codex/codex-rs/windows-sandbox-rs/src/elevated_impl.rs`
  - `CreateProcessWithLogonW` runner launch.
- `~/Projects/github/codex/codex-rs/windows-sandbox-rs/src/elevated/command_runner_win.rs`
  - sandbox-user command runner and restricted child creation.
- `~/Projects/github/codex/codex-rs/windows-sandbox-rs/src/acl.rs`
  - ACL mask checks and write-deny helpers.
- `~/Projects/github/codex/codex-rs/windows-sandbox-rs/src/audit.rs`
  - world-writable write-hole mitigation.
- `~/Projects/github/codex/codex-rs/windows-sandbox-rs/src/token.rs`
  - restricted-token capability model.

## Windows-Only Work Remaining

1. Validate setup identity primitives live:
   - local group `AhandSandboxUsers`
   - users `AhandSandboxOnline` and `AhandSandboxOffline`
   - DPAPI-protected state in `.sandbox-secrets`
   - SID resolution for users and group

2. Validate network semantics live:
   - Online user can access network.
   - Offline user cannot access public IPs, DNS, or loopback escape paths.
   - Firewall verification fails closed if rules are absent or too narrow.

3. Implement and validate the runner/logon model:
   - Add Codex-style runner flow using `CreateProcessWithLogonW`.
   - Runner must execute as the selected sandbox user.
   - Child process must use restricted token/capability constraints.
   - The capture path must consume `WindowsNetworkContext.sandbox_creds`; it must not discard creds and fall back to current process token.

4. Validate ACLs live:
   - write roots allow intended writes only.
   - read roots allow intended reads/execute only.
   - sandbox state and secrets are not granted as user-accessible roots.
   - `NUL` works for redirected stdio.
   - inherited ACLs behave correctly for nested files/directories.

5. Add world-writable hardening:
   - Align with Codex `audit.rs`.
   - Deny write holes outside workspace roots where global writable ACLs would bypass the sandbox.

6. Only after the above, consider changing `process_execution_enabled()` to true.

## Suggested Windows Validation Commands

From the aHand checkout on Windows:

```powershell
git fetch origin
git checkout codex/windows-coffice-sandbox
cargo test -p ahandd sandbox::platform::windows
cargo test -p ahandd sandbox
cargo check -p ahandd --target x86_64-pc-windows-msvc
cargo check -p ahandd --tests --target x86_64-pc-windows-msvc
cargo fmt --check
git diff --check
```

After implementing runner/logon and enabling execution, add live execution tests for:

- `NetworkPolicy::Enabled` command can reach network.
- `NetworkPolicy::Disabled` command cannot reach network.
- command can write inside workspace.
- command cannot write outside workspace.
- command can read configured runtime roots.
- command cannot read or write `.sandbox-secrets`.
- Coffice smoke path still passes through the platform-neutral sandbox API.

## Stop Conditions

Stop and report instead of forcing through if any of these happen:

- Windows local user setup requires elevation flow that is not yet wired.
- Firewall rules cannot be verified live.
- ACL checks pass only because the current developer/admin user has broad access.
- Execution can only work by using current-process token instead of sandbox user logon.
- `NetworkPolicy::Disabled` permits any non-approved outbound path.
- macOS sandbox tests regress.

## Merge And Return Notes

After Windows validation is complete:

1. Commit the Windows changes on `codex/windows-coffice-sandbox`.
2. Push the branch.
3. Merge according to the current project convention. Earlier discussion mentioned aHand `dev`, then clarified it should likely be aHand `main`; confirm the target branch before merging.
4. After testing and merge, switch the Windows/local working tree back to the branch/context it started from.
