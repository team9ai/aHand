# Sandbox Run Command Shell Semantics Design

**Date:** 2026-07-01
**Status:** Approved approach B, design review pending
**Base:** `codex/ahand-sandbox-tool-provider` at `56d1b48`
**Scope:** aHand sandbox tool provider and Coffice-facing sandbox command semantics.

## Overview

The current aHand sandbox `run_command` tool is too narrow. It accepts `command: string[]`
and resolves `command[0]` only against executable directories contributed by registered runtime
providers. This means Coffice can run registered runtimes such as Python and Node, but it cannot
use normal sandbox-local commands such as `find`, `rg`, `git`, `sh`, PowerShell, or `cmd` unless
they are modeled as runtime providers.

That is the wrong boundary. The sandbox boundary should be filesystem, network, process, mount,
and commit policy. It should not be a runtime executable whitelist. Codex's command model is the
reference: the model-facing command tool accepts a command string, the host wraps it with the
platform shell, and OS sandboxing plus approval policy enforce what the process can read, write,
or reach over the network.

This design changes aHand toward that model while preserving compatibility for existing callers.

## Goals

- Make `run_command` useful for normal sandbox work: shell pipelines, file search, simple file
  transforms, runtime commands, and platform-native command syntax.
- Keep one command tool. Do not introduce a separate `run_shell` tool.
- Preserve existing callers that send `command: string[]` for one migration window.
- Keep sandbox policy as the authority for privilege boundaries.
- Keep Coffice insulated from platform-specific shell selection.

## Non-Goals

- Do not implement a full Codex exec policy or approval rule engine in this change.
- Do not allow commands to read or write outside the sandbox policy.
- Do not remove `sandbox_exec` or `run_node` compatibility aliases in this change.
- Do not require Coffice or agent-pi to know whether the local host is using `sh`, `zsh`,
  PowerShell, or `cmd`.

## Current Behavior

`SandboxToolProvider::run_command` accepts:

```json
{
  "command": ["node", "-e", "console.log(1)"],
  "cwd": ".",
  "env": {},
  "timeoutSeconds": 30
}
```

The public API then splits the vector into `program` and `args`, calls
`runner::resolve_executable(program, exec_env.path_entries)`, and rejects any program that is not
inside registered runtime PATH entries. `sh` fails with `COMMAND_NOT_FOUND` because Coffice only
registers Python and Node runtime providers.

## Target Tool Contract

`run_command` accepts exactly one of `cmd` or `command`.

`cmd` is the preferred field:

```json
{
  "cmd": "find . -name '*.xlsx' | head",
  "cwd": ".",
  "env": {},
  "timeoutSeconds": 30
}
```

`command` remains a compatibility field:

```json
{
  "command": ["node", "-e", "console.log(1)"],
  "cwd": ".",
  "env": {},
  "timeoutSeconds": 30
}
```

Validation rules:

- If neither `cmd` nor `command` is present, return `INVALID_ARGUMENT`.
- If both are present, return `INVALID_ARGUMENT`.
- `cmd` must be a non-empty string after trimming.
- `command` must be a non-empty array of strings.
- `cwd`, `env`, and `timeoutSeconds` keep their existing meanings.

The `sandbox_exec` compatibility alias uses the same schema and handler as `run_command`.
`run_node` remains a wrapper that builds the legacy `command` array.

## Execution Model

Add an internal command representation:

```rust
pub enum SandboxCommand {
    Shell { cmd: String },
    Argv { command: Vec<String> },
}
```

The shell path is selected inside aHand:

- macOS and Linux: use the user shell if it is a usable `zsh`, `bash`, or `sh`; otherwise use
  `/bin/sh`.
- Windows: prefer `pwsh.exe`, then `powershell.exe`, then `cmd.exe`.

The selected shell converts `SandboxCommand::Shell` into argv:

- POSIX: `[shell_path, "-c", cmd]`
- PowerShell: `[powershell_path, "-NoProfile", "-Command", cmd]`
- cmd.exe: `[cmd_path, "/c", cmd]`

`SandboxCommand::Argv` passes through as direct argv for compatibility. It must not be resolved
only against runtime provider PATH entries. The platform runner receives argv and an environment
whose PATH includes runtime provider paths before the inherited or platform default PATH.

## Sandbox Policy

Runtime providers keep their current purpose:

- Contribute PATH entries.
- Contribute environment variables.
- Contribute readonly roots for runtime libraries.
- Contribute default timeout hints.

Runtime providers do not define the complete executable allowlist.

The sandbox policy remains responsible for authority:

- The session workspace root is writable.
- Runtime roots are readonly.
- Selected folder and host-file mounts are added according to their registered access mode.
- Network follows the session network policy.
- Commit back to host files still requires `register_file_version` and `commit_file_version`.

Platform runners must include the readonly roots needed to start platform shell and basic system
tools. On macOS this includes the existing system readonly roots such as `/bin`, `/usr/bin`,
`/usr/lib`, `/usr/libexec`, and system frameworks, plus common managed tool roots such as
`/opt/homebrew` and `/usr/local` when those paths exist. On Windows this maps to readonly ACLs for
the selected shell, system directories, runtime roots, and installed tool roots used by PATH.

The process may execute shell builtins and child commands, but the child processes inherit the
same sandbox policy. Running `rm`, `curl`, `osascript`, PowerShell cmdlets, or `cmd /c` is not a
boundary escape by itself; the sandbox decides whether the operation can touch protected paths or
network destinations.

## Error Semantics

Keep stable errors:

- `INVALID_ARGUMENT`: malformed tool input.
- `INVALID_COMMAND`: empty shell command, empty argv, unsupported shell selection, or command
  construction failure.
- `COMMAND_NOT_FOUND`: the platform cannot locate the selected shell or direct argv executable.
- `PERMISSION_DENIED`: sandbox denied filesystem, process, or network access.
- `SANDBOX_UNAVAILABLE`: platform sandbox cannot run.

The previous message "not found in registered runtime PATH" should not appear for `cmd` commands.
For legacy `command` array calls, command lookup failures should mention the effective sandbox
PATH, not runtime registration.

## Coffice Impact

Coffice should keep registering Python and Node runtime providers so bundled libraries and PATH
entries remain available inside the sandbox.

Coffice and agent prompts should prefer:

```json
{ "cmd": "python - <<'PY'\nprint('ok')\nPY" }
```

instead of:

```json
{ "command": ["python", "-c", "print('ok')"] }
```

The desktop stream renderer already displays `run_command` and historical `sandbox_exec` events.
No UI split between command and shell is needed.

## Tests

aHand unit tests:

- `run_command` accepts `cmd` and forwards a shell command request.
- `run_command` accepts legacy `command` and forwards an argv command request.
- `run_command` rejects both `cmd` and `command`.
- `run_command` rejects neither `cmd` nor `command`.
- shell selection returns POSIX argv on macOS/Linux and PowerShell/cmd argv on Windows.
- direct argv no longer uses registered runtime PATH as its only resolution source.

aHand macOS integration tests:

- `cmd: "printf ok > out.txt"` writes inside the workspace.
- `cmd: "find . -name out.txt"` can read inside the workspace.
- `cmd: "cat /etc/passwd"` fails under restricted sandbox policy.
- `cmd: "node -e \"console.log(1)\""` still works when Coffice registers Node.

Coffice tests:

- App-tool catalog schema accepts `cmd`.
- Local E2E invokes `run_command` with `cmd` for a shell pipeline.
- Existing `command` array E2E still works for one migration window.

## Migration

1. Add `cmd` support in aHand while keeping `command`.
2. Update aHand tool descriptors to mark `cmd` as preferred.
3. Update Coffice prompts and local E2E to use `cmd`.
4. Update Coffice's aHand git pin after aHand tests pass.
5. Keep `command`, `sandbox_exec`, and `run_node` compatibility aliases until downstream agents
   have stopped emitting them.
