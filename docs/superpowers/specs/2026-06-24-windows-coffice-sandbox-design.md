# Windows Coffice Sandbox Design

**Date:** 2026-06-24
**Status:** Draft for user review
**Scope:** aHand sandbox Windows backend for Coffice, with platform-neutral API semantics preserved across macOS and Windows.

## Goal

The first delivery target is Coffice running against aHand sandbox on native Windows through the same platform-neutral `ahandd::DaemonHandle` APIs it already uses on macOS.

The goal hierarchy is:

- **Current first step:** make the Coffice sandbox workflow work end to end on Windows.
- **Long-term target:** make this a generic aHand Windows sandbox backend, not a Coffice-only shim.
- **Implementation principle:** follow Codex's Windows sandbox model where it defines the right security shape.

Coffice must not need OS-specific sandbox tool code for normal use.

## Selected Approach

Use **Coffice-first, Codex-aligned core**.

This means first implementing the Windows backend pieces that are required for Coffice's workflow and that match the core Codex isolation model:

- restricted token
- capability SID
- ACL-based writable workspace/session root
- readonly/execute runtime roots
- process creation with captured stdio
- timeout termination
- hard network policy semantics consistent with macOS

This does not mean porting every Codex subsystem in the first phase. Codex's broader allow/deny path computation, workspace-specific hardening, elevated setup orchestration, and proxy-only network mode can be added later, as long as the first phase does not create API semantics that conflict with them.

## Existing Shared aHand Sandbox Surface

These modules are already platform-neutral and should remain so:

- `crates/ahandd/src/public_api.rs`
  Exposes `DaemonHandle` sandbox methods for embedded consumers such as Coffice.

- `crates/ahandd/src/sandbox/types.rs`
  Defines session config, runtime provider config, permission mode, network policy, command/runtime requests, execution result, file version, and commit result types.

- `crates/ahandd/src/sandbox/registry.rs`
  Tracks sandbox sessions, runtime providers, permissions, host file refs, imported files, and aggregate execution environment.

- `crates/ahandd/src/sandbox/path_policy.rs`
  Validates sandbox-relative paths and prevents traversal outside the sandbox root.

- `crates/ahandd/src/sandbox/file_lifecycle.rs`
  Owns host-file import, sandbox candidate version registration, commit, overwrite confirmation, and save-as.

- `crates/ahandd/src/sandbox/runner.rs`
  Resolves executables and dispatches to the platform backend.

Windows work belongs under `crates/ahandd/src/sandbox/platform/windows`.

## Coffice Data Flow

Coffice should keep its current shape:

1. `sandbox_tools.rs` exposes tool calls such as `import_file`, `sandbox_exec`, `run_python`, `run_node`, `register_file_version`, and `commit_file_version`.
2. `sandbox_adapter.rs` calls the `ahandd::DaemonHandle` sandbox methods.
3. aHand shared sandbox modules manage session state, path validation, runtime registration, file lifecycle, and command dispatch.
4. The platform backend executes commands with OS-specific isolation.

File flow:

- Coffice host file references enter aHand through `import_sandbox_file`.
- aHand copies/imports them into the sandbox.
- Python/Node/sandbox commands operate only inside the sandbox workspace/session root.
- Generated candidate files are registered through `register_sandbox_file_version`.
- Host writes occur only through aHand-controlled `commit`, overwrite confirmation, or save-as APIs.

## Platform-Neutral API Semantics

Coffice must see the same sandbox API behavior on macOS and Windows.

The backend implementation can differ, but these semantics must not differ:

- Execution result shape: `stdout`, `stderr`, `exit_code`, and `timed_out`.
- File lifecycle behavior: import, candidate registration, commit, overwrite confirmation, and save-as.
- Error classification: use stable sandbox error codes such as `COMMAND_NOT_FOUND`, `INVALID_COMMAND`, `PERMISSION_DENIED`, and `SANDBOX_UNAVAILABLE`.
- Runtime registration and command resolution: Coffice should not need to know whether the runtime executable is `python` or `python.exe`; aHand handles platform executable lookup.
- Network policy behavior.

## Network Policy

`NetworkPolicy` is part of the platform-neutral API contract.

First-phase behavior:

- `NetworkPolicy::Enabled`
  - macOS: include `allow network*` in the sandbox policy.
  - Windows: allow real network access.

- `NetworkPolicy::Disabled`
  - macOS: continue to omit `allow network*` from the `sandbox-exec` policy.
  - Windows: enforce hard network blocking using Codex's Windows approach, not just environment cleanup.
  - If Windows cannot enforce this, command execution must fail closed.

- `NetworkPolicy::ProxyOnly`
  - First phase: unsupported on both macOS and Windows.
  - Both platforms should return the same unsupported sandbox error.
  - A later phase can define and implement proxy-only behavior for both platforms together.

Any future change to network policy semantics must update macOS and Windows together.

## File System Isolation

File system isolation should align with Codex's core Windows model for the first phase:

- The sandbox workspace/session root is writable.
- Runtime/provider roots are readonly and executable.
- Host files are not directly writable by sandboxed child processes.
- Host writes are controlled by aHand file lifecycle APIs.
- Coffice should not need to branch on OS for file handling.

Windows should implement this with restricted token + capability SID + ACL grants.

macOS should keep its existing `sandbox-exec` policy behavior and must not regress.

## macOS Non-Regression

The existing macOS backend is already functional and must keep its current behavior:

- Keep `platform/macos.rs` as the macOS execution path.
- Do not relax shared path policy or file lifecycle behavior for Windows convenience.
- Non-Windows command resolution must remain unchanged except for tests that prove it stayed unchanged.
- Every implementation slice must run macOS sandbox regression tests.

## Windows Backend Components

The Windows backend should be organized under `crates/ahandd/src/sandbox/platform/windows`:

- `mod.rs`
  Dispatches execution into the backend and maps backend errors into `SandboxError`.

- `cap.rs`
  Creates or loads a stable capability SID for the sandbox root.

- `token.rs`
  Creates the restricted token with the capability SID.

- `acl.rs`
  Grants writable access to the workspace/session root and readonly/execute access to runtime roots.

- `env.rs`
  Normalizes process environment and supports the network policy implementation.

- `path.rs`
  Converts paths/strings for Win32 calls and handles Windows absolute path normalization.

- `process.rs`
  Launches the restricted child process, captures stdio, tracks exit code, and terminates on timeout.

- `capture.rs`
  Wires capability, ACL, token, environment, process launch, stdio capture, timeout, and result mapping together.

## Error Handling

The backend should fail closed:

- Restricted token setup failure: do not execute the command.
- ACL setup failure: do not execute the command.
- Network policy enforcement failure for `Disabled`: do not execute the command.
- Runtime executable not found: return `COMMAND_NOT_FOUND`.
- Absolute executable outside registered runtime path: return `INVALID_COMMAND`.
- Unsupported `ProxyOnly`: return a stable unsupported sandbox error on both macOS and Windows.
- Timeout: terminate the child process and return `timed_out = true`.

Windows-specific diagnostic detail can be included in the error message, but Coffice should not need to branch on raw Win32 errors.

## Coffice Dependency Pinning

Coffice may temporarily pin `ahandd` to the Windows sandbox feature branch for validation.

After the aHand implementation merges to aHand `main`, Coffice must not remain pinned to the temporary feature branch. It should move back to aHand `main`, a main commit, or a release tag.

## Acceptance Criteria

### aHand macOS

- `cargo test -p ahandd sandbox`
- `cargo test -p ahandd --test sandbox_api`
- `cargo test -p ahandd --test sandbox_smoke`

### aHand Windows

- `cargo check -p ahandd --target x86_64-pc-windows-msvc`
- Windows runner: `cargo test -p ahandd --test sandbox_api`
- Windows runner: Coffice-shaped sandbox execution test covering Python/Node/command execution.
- Windows runner: network policy tests for `Enabled`, `Disabled`, and unsupported `ProxyOnly`.

### Coffice Windows

Using Coffice's real adapter/tool/runtime path:

- `import_file` works.
- `run_python` works.
- `run_node` works.
- `sandbox_exec` works.
- `register_file_version` works.
- `commit_file_version` works.
- `save_as` works.
- `NetworkPolicy::Disabled` blocks real network access.
- `NetworkPolicy::ProxyOnly` returns unsupported consistently.

## Out of Scope for First Phase

- Full Codex allow/deny path parity beyond the core writable workspace and readonly runtime roots.
- Codex workspace-specific protections such as `.git`, `.codex`, and `.agents` mapping, unless required to enforce the agreed Coffice first-phase boundary.
- Interactive desktop sandboxing.
- Windows service or daemon lifecycle work.
- Permanent Coffice pin to an aHand feature branch.
