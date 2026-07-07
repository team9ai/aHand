# Windows Sandbox Startup Prompt

Copy this into the Windows machine's Codex session.

```text
We are continuing aHand Windows sandbox work.

Repository:
- team9ai/aHand
- Branch: codex/windows-coffice-sandbox
- Base: origin/dev
- Latest implementation commit before the handoff docs: 081cf6e feat(sandbox): wire Windows sandbox readiness

Reference repo:
- ~/Projects/github/codex
- Focus on ~/Projects/github/codex/codex-rs/windows-sandbox-rs
- Key files: setup_orchestrator.rs, setup_main_win.rs, elevated_impl.rs, elevated/command_runner_win.rs, process.rs, acl.rs, audit.rs, token.rs

Read first:
- docs/superpowers/handoffs/2026-06-25-windows-sandbox-handoff.md
- crates/ahandd/src/sandbox/platform/windows/capture.rs
- crates/ahandd/src/sandbox/platform/windows/setup.rs
- crates/ahandd/src/sandbox/platform/windows/identity.rs
- crates/ahandd/src/sandbox/platform/windows/sandbox_users.rs
- crates/ahandd/src/sandbox/platform/windows/firewall.rs
- crates/ahandd/src/sandbox/platform/windows/roots.rs
- crates/ahandd/src/sandbox/platform/windows/acl.rs
- crates/ahandd/src/sandbox/platform/windows/process.rs
- crates/ahandd/src/sandbox/platform/windows/token.rs

User intent:
- Finish Windows adaptation for the current aHand sandbox.
- Focus on the Coffice/aHand upper-layer integration point: upper apps should keep using the platform-neutral sandbox API and RuntimeSandboxPolicy.
- NetworkPolicy::Enabled means real network allowed.
- NetworkPolicy::Disabled means strict hard network block.
- ProxyOnly remains unsupported for now.
- Filesystem behavior should align with Codex.
- macOS must not regress.

Current implementation boundary:
- process_execution_enabled() is false.
- Do not flip process_execution_enabled() by itself.
- Do not reintroduce direct current-process-token launch as the enabled path.
- capture.rs now intentionally fails closed at resolve_sandbox_user_runner_launch() until a Codex-style sandbox user runner/logon flow exists.
- The enabled path must consume WindowsNetworkContext.sandbox_creds and run through sandbox user logon, not discard creds.

Immediate Windows tasks:
1. Run baseline checks:
   cargo test -p ahandd sandbox::platform::windows
   cargo test -p ahandd sandbox
   cargo check -p ahandd --target x86_64-pc-windows-msvc
   cargo check -p ahandd --tests --target x86_64-pc-windows-msvc
   cargo fmt --check
   git diff --check

2. Live-validate local setup primitives:
   - AhandSandboxUsers group
   - AhandSandboxOnline user
   - AhandSandboxOffline user
   - DPAPI-protected .sandbox-secrets state
   - SID resolution for users/group

3. Live-validate NetworkPolicy semantics:
   - Enabled user can access network.
   - Disabled user cannot reach public IPs, DNS, or loopback bypasses.
   - firewall verification fails closed when rules are missing or wrong.

4. Implement Codex-style runner/logon:
   - Use CreateProcessWithLogonW or the Codex-equivalent helper flow.
   - Runner should execute as the selected sandbox local user.
   - Child should run with restricted token/capability constraints.
   - stdout/stderr capture, timeout, job cleanup, .cmd/.bat quoting, env block validation must remain covered.

5. Live-validate filesystem isolation:
   - writes inside workspace allowed.
   - writes outside workspace denied.
   - reads from configured runtime roots allowed.
   - sandbox state/secrets denied.
   - ACLs use DerivedFilesystemRoots, not raw policy roots.
   - world-writable write holes are handled, aligned with Codex audit.rs.

6. Only after all live validations pass, change process_execution_enabled() to true and add tests proving it.

Stop conditions:
- If the only way to run is current-process token, stop.
- If Disabled network is not strictly blocked, stop.
- If ACL validation depends on admin/current-user broad access, stop.
- If elevation/helper flow is required but not wired, stop and document the blocker.
- If macOS sandbox tests regress, stop.

Expected completion:
- Commit Windows changes.
- Push codex/windows-coffice-sandbox.
- Confirm merge target before merging; earlier discussion mentioned dev, then clarified likely aHand main.
- After merge/testing, switch the local Windows checkout back to its previous branch/context.
```
