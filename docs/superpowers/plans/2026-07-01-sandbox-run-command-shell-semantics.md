# Sandbox Run Command Shell Semantics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `run_command` accept a Codex-style shell command string while keeping legacy argv compatibility and preserving the platform sandbox as the authority boundary.

**Architecture:** `run_command` and `sandbox_exec` share one parser that accepts exactly one of `cmd: string` or legacy `command: string[]`. aHand converts `cmd` into the platform shell argv inside the runner, merges registered runtime PATH/env/readonly roots, then delegates execution to the macOS or Windows sandbox backend without treating runtime PATH as a command allowlist.

**Tech Stack:** Rust, tokio, serde_json, macOS `sandbox-exec`, aHand app-tool registry, Coffice Tauri Cargo git dependency.

---

## File Structure

- Modify `crates/ahandd/src/sandbox/types.rs`: add `SandboxCommand` and change `SandboxExecRequest.command` from `Vec<String>` to `SandboxCommand`.
- Modify `crates/ahandd/src/sandbox/mod.rs`: re-export `SandboxCommand`.
- Modify `crates/ahandd/src/sandbox/tool_provider.rs`: parse `cmd` or legacy `command`, update schema, keep `sandbox_exec` alias and `run_node` wrapper.
- Modify `crates/ahandd/src/sandbox/runner.rs`: replace runtime PATH executable resolution with command construction helpers for shell strings and direct argv.
- Modify `crates/ahandd/src/public_api.rs`: match `SandboxCommand`, merge runtime PATH/env, and pass a complete argv to the platform runner.
- Modify `crates/ahandd/src/sandbox/platform/macos.rs`: pass the complete sandboxed argv through `sandbox-exec -- ...` and allow common system/Homebrew executable roots.
- Modify `crates/ahandd/src/sandbox/platform/windows.rs`: compile with the new `PlatformExecuteRequest` shape; Windows execution remains fail-closed until the backend lands.
- Modify `crates/ahandd/tests/sandbox_smoke.rs`: wrap direct argv calls in `SandboxCommand::Argv`.
- Later in Coffice, modify `apps/desktop/src-tauri/Cargo.toml` and `apps/desktop/src-tauri/Cargo.lock`: pin to the new aHand commit after aHand tests pass.

### Task 1: Request Model And Tool Parser

**Files:**
- Modify: `crates/ahandd/src/sandbox/types.rs`
- Modify: `crates/ahandd/src/sandbox/mod.rs`
- Modify: `crates/ahandd/src/sandbox/tool_provider.rs`

- [ ] **Step 1: Write failing type and parser tests**

Add tests proving the desired public contract before changing production behavior:

```rust
// crates/ahandd/src/sandbox/types.rs
#[test]
fn sandbox_exec_request_keeps_shell_command_cwd_env_and_timeout() {
    let request = SandboxExecRequest {
        command: SandboxCommand::Shell {
            cmd: "echo ok".to_string(),
        },
        cwd: Some(PathBuf::from("workspace")),
        env: HashMap::from([("EXAMPLE".to_string(), "1".to_string())]),
        timeout: Some(Duration::from_secs(7)),
    };

    assert_eq!(
        request.command,
        SandboxCommand::Shell {
            cmd: "echo ok".to_string()
        }
    );
    assert_eq!(request.cwd, Some(PathBuf::from("workspace")));
    assert_eq!(request.env["EXAMPLE"], "1");
    assert_eq!(request.timeout, Some(Duration::from_secs(7)));
}
```

```rust
// crates/ahandd/src/sandbox/tool_provider.rs
#[tokio::test]
async fn run_command_accepts_shell_cmd_request() {
    let captured_exec = Arc::new(AsyncMutex::new(None));
    let provider = SandboxToolProvider::new_for_test_with_exec_capture(
        Arc::new(AsyncMutex::new(SandboxRegistry::default())),
        Arc::new(FixedSandboxInvocationResolver::new("session-1")),
        SandboxToolProviderOptions {
            include_compat_aliases: true,
        },
        Arc::clone(&captured_exec),
    );

    let result = handler(&provider, "run_command")(invocation(
        json!({
            "cmd": "echo ok",
            "cwd": "workspace",
            "env": { "EXAMPLE": "1" },
            "timeoutSeconds": 7
        }),
        Some(trusted_context()),
    ))
    .await
    .unwrap();

    assert_eq!(result["exitCode"], json!(0));
    let captured = captured_exec.lock().await.clone().unwrap();
    assert_eq!(
        captured.command,
        SandboxCommand::Shell {
            cmd: "echo ok".to_string()
        }
    );
    assert_eq!(captured.cwd, Some(PathBuf::from("workspace")));
    assert_eq!(captured.env["EXAMPLE"], "1");
    assert_eq!(captured.timeout, Some(Duration::from_secs(7)));
}

#[tokio::test]
async fn run_command_accepts_legacy_argv_request() {
    let captured_exec = Arc::new(AsyncMutex::new(None));
    let provider = SandboxToolProvider::new_for_test_with_exec_capture(
        Arc::new(AsyncMutex::new(SandboxRegistry::default())),
        Arc::new(FixedSandboxInvocationResolver::new("session-1")),
        SandboxToolProviderOptions {
            include_compat_aliases: true,
        },
        Arc::clone(&captured_exec),
    );

    handler(&provider, "sandbox_exec")(invocation(
        json!({"command": ["python", "-c", "print('ok')"]}),
        Some(trusted_context()),
    ))
    .await
    .unwrap();

    let captured = captured_exec.lock().await.clone().unwrap();
    assert_eq!(
        captured.command,
        SandboxCommand::Argv {
            command: vec!["python".to_string(), "-c".to_string(), "print('ok')".to_string()]
        }
    );
}

#[tokio::test]
async fn run_command_rejects_cmd_and_command_together() {
    let captured_exec = Arc::new(AsyncMutex::new(None));
    let provider = SandboxToolProvider::new_for_test_with_exec_capture(
        Arc::new(AsyncMutex::new(SandboxRegistry::default())),
        Arc::new(FixedSandboxInvocationResolver::new("session-1")),
        SandboxToolProviderOptions {
            include_compat_aliases: true,
        },
        Arc::clone(&captured_exec),
    );

    let err = handler(&provider, "run_command")(invocation(
        json!({"cmd": "echo ok", "command": ["echo", "ok"]}),
        Some(trusted_context()),
    ))
    .await
    .unwrap_err();

    assert_eq!(err.code, "INVALID_ARGUMENT");
    assert!(captured_exec.lock().await.is_none());
}

#[tokio::test]
async fn run_command_rejects_missing_cmd_and_command() {
    let captured_exec = Arc::new(AsyncMutex::new(None));
    let provider = SandboxToolProvider::new_for_test_with_exec_capture(
        Arc::new(AsyncMutex::new(SandboxRegistry::default())),
        Arc::new(FixedSandboxInvocationResolver::new("session-1")),
        SandboxToolProviderOptions {
            include_compat_aliases: true,
        },
        Arc::clone(&captured_exec),
    );

    let err = handler(&provider, "run_command")(invocation(
        json!({"cwd": "workspace"}),
        Some(trusted_context()),
    ))
    .await
    .unwrap_err();

    assert_eq!(err.code, "INVALID_ARGUMENT");
    assert!(captured_exec.lock().await.is_none());
}
```

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test -p ahandd sandbox::types::tests::sandbox_exec_request_keeps_shell_command_cwd_env_and_timeout
cargo test -p ahandd sandbox::tool_provider::tests::run_command_accepts_shell_cmd_request
```

Expected: FAIL because `SandboxCommand` does not exist and `run_command` still requires `command`.

- [ ] **Step 3: Add minimal request model and parser**

Implement this shape:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxCommand {
    Shell { cmd: String },
    Argv { command: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxExecRequest {
    pub command: SandboxCommand,
    pub cwd: Option<PathBuf>,
    pub env: HashMap<String, String>,
    pub timeout: Option<Duration>,
}
```

In `tool_provider.rs`, add a parser with these exact branches:

```rust
fn parse_sandbox_command_arg(args: &Value) -> Result<SandboxCommand, AppToolError> {
    let has_cmd = args.get("cmd").is_some();
    let has_command = args.get("command").is_some();
    match (has_cmd, has_command) {
        (true, false) => {
            let cmd = require_string_arg(args, "cmd")?;
            if cmd.trim().is_empty() {
                return Err(AppToolError::invalid_argument("cmd must not be empty"));
            }
            Ok(SandboxCommand::Shell { cmd })
        }
        (false, true) => Ok(SandboxCommand::Argv {
            command: require_non_empty_string_array_arg(args, "command")?,
        }),
        (true, true) => Err(AppToolError::invalid_argument(
            "provide exactly one of cmd or command",
        )),
        (false, false) => Err(AppToolError::invalid_argument(
            "provide exactly one of cmd or command",
        )),
    }
}
```

Update `run_command` to call `parse_sandbox_command_arg`, and update `run_node` to build:

```rust
let command = SandboxCommand::Argv {
    command: std::iter::once("node".to_string()).chain(args).collect(),
};
```

Update `run_command_def` so `cmd` and legacy `command` are documented in one schema with no top-level `required`:

```rust
"properties": {
    "cmd": { "type": "string", "minLength": 1 },
    "command": {
        "type": "array",
        "items": { "type": "string" },
        "minItems": 1
    },
    ...
},
"oneOf": [
    { "required": ["cmd"], "not": { "required": ["command"] } },
    { "required": ["command"], "not": { "required": ["cmd"] } }
],
"additionalProperties": false
```

- [ ] **Step 4: Run tests and verify GREEN**

Run:

```bash
cargo test -p ahandd sandbox::types::tests::sandbox_exec_request_keeps_shell_command_cwd_env_and_timeout
cargo test -p ahandd sandbox::tool_provider::tests::run_command_accepts_shell_cmd_request
cargo test -p ahandd sandbox::tool_provider::tests::run_command_accepts_legacy_argv_request
cargo test -p ahandd sandbox::tool_provider::tests::run_command_rejects_cmd_and_command_together
cargo test -p ahandd sandbox::tool_provider::tests::run_command_rejects_missing_cmd_and_command
```

Expected: PASS.

### Task 2: Runner Command Construction

**Files:**
- Modify: `crates/ahandd/src/sandbox/runner.rs`

- [ ] **Step 1: Write failing runner tests**

Replace the runtime-PATH allowlist tests with command-construction tests:

```rust
#[cfg(unix)]
#[test]
fn shell_command_uses_posix_shell_c() {
    let command = command_argv_from_sandbox_command(&SandboxCommand::Shell {
        cmd: "echo ok".to_string(),
    })
    .unwrap();

    assert_eq!(command.len(), 3);
    assert_eq!(command[1], "-c");
    assert_eq!(command[2], "echo ok");
    assert!(
        command[0].ends_with("/zsh")
            || command[0].ends_with("/bash")
            || command[0].ends_with("/sh")
    );
}

#[test]
fn argv_command_passes_through_without_runtime_resolution() {
    let command = command_argv_from_sandbox_command(&SandboxCommand::Argv {
        command: vec!["python".to_string(), "-c".to_string(), "print('ok')".to_string()],
    })
    .unwrap();

    assert_eq!(command, vec!["python", "-c", "print('ok')"]);
}

#[test]
fn empty_argv_command_is_invalid() {
    let err = command_argv_from_sandbox_command(&SandboxCommand::Argv { command: vec![] })
        .unwrap_err();

    assert_eq!(err.code, "INVALID_COMMAND");
}

#[test]
fn blank_shell_command_is_invalid() {
    let err = command_argv_from_sandbox_command(&SandboxCommand::Shell {
        cmd: "   ".to_string(),
    })
    .unwrap_err();

    assert_eq!(err.code, "INVALID_COMMAND");
}
```

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test -p ahandd sandbox::runner::tests::shell_command_uses_posix_shell_c
cargo test -p ahandd sandbox::runner::tests::argv_command_passes_through_without_runtime_resolution
```

Expected: FAIL because `command_argv_from_sandbox_command` does not exist.

- [ ] **Step 3: Implement command construction**

Change `PlatformExecuteRequest` to:

```rust
pub struct PlatformExecuteRequest {
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    pub timeout: Duration,
    pub policy: RuntimeSandboxPolicy,
}
```

Add:

```rust
pub fn command_argv_from_sandbox_command(command: &SandboxCommand) -> SandboxResult<Vec<String>> {
    match command {
        SandboxCommand::Shell { cmd } => shell_argv(cmd),
        SandboxCommand::Argv { command } => {
            if command.is_empty() || command[0].trim().is_empty() {
                return Err(SandboxError::invalid_command("sandbox command must not be empty"));
            }
            Ok(command.clone())
        }
    }
}

#[cfg(unix)]
fn shell_argv(cmd: &str) -> SandboxResult<Vec<String>> {
    if cmd.trim().is_empty() {
        return Err(SandboxError::invalid_command("cmd must not be empty"));
    }
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|value| {
            value.ends_with("/zsh") || value.ends_with("/bash") || value.ends_with("/sh")
        })
        .filter(|value| Path::new(value).exists())
        .unwrap_or_else(|| "/bin/sh".to_string());
    Ok(vec![shell, "-c".to_string(), cmd.to_string()])
}

#[cfg(windows)]
fn shell_argv(cmd: &str) -> SandboxResult<Vec<String>> {
    if cmd.trim().is_empty() {
        return Err(SandboxError::invalid_command("cmd must not be empty"));
    }
    if let Some(shell) = find_windows_shell("pwsh.exe")
        .or_else(|| find_windows_shell("powershell.exe"))
    {
        return Ok(vec![
            shell,
            "-NoProfile".to_string(),
            "-Command".to_string(),
            cmd.to_string(),
        ]);
    }
    if let Some(shell) = find_windows_shell("cmd.exe") {
        return Ok(vec![shell, "/c".to_string(), cmd.to_string()]);
    }
    Err(SandboxError::command_not_found(
        "no Windows shell found for sandbox command",
    ))
}
```

Keep `find_windows_shell` small and PATH-based; do not introduce command execution during lookup.

- [ ] **Step 4: Run tests and verify GREEN**

Run:

```bash
cargo test -p ahandd sandbox::runner
```

Expected: PASS.

### Task 3: Public API Wiring

**Files:**
- Modify: `crates/ahandd/src/public_api.rs`

- [ ] **Step 1: Write failing public API tests**

Update `execute_sandbox_command_rejects_empty_command` to use `SandboxCommand::Argv { command: vec![] }`, then add:

```rust
#[tokio::test]
async fn execute_sandbox_command_accepts_shell_command_model() {
    let temp = tempfile::tempdir().unwrap();
    let identity_dir = temp.path().join("identity");
    let workspace_root = temp.path().join("sandbox");
    std::fs::create_dir_all(&workspace_root).unwrap();
    let cfg = DaemonConfig::builder("ws://127.0.0.1:9/ws", "test-token", &identity_dir)
        .heartbeat_interval(Duration::from_millis(50))
        .build();
    let handle = spawn(cfg).await.unwrap();
    handle
        .create_sandbox_session(SandboxSessionConfig {
            session_id: "session-1".to_string(),
            permission_mode: SandboxPermissionMode::Readonly,
            workspace_root,
            network: NetworkPolicy::Enabled,
        })
        .await
        .unwrap();

    let result = handle
        .execute_sandbox_command(
            "session-1",
            SandboxExecRequest {
                command: SandboxCommand::Shell {
                    cmd: "echo ok".to_string(),
                },
                cwd: None,
                env: HashMap::new(),
                timeout: Some(Duration::from_secs(5)),
            },
        )
        .await;

    #[cfg(target_os = "macos")]
    assert_eq!(result.unwrap().stdout.trim(), "ok");
    #[cfg(not(target_os = "macos"))]
    assert_eq!(result.unwrap_err().code, "SANDBOX_UNAVAILABLE");

    handle.shutdown().await.unwrap();
}
```

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test -p ahandd public_api::tests::execute_sandbox_command_accepts_shell_command_model
```

Expected: FAIL because `execute_sandbox_command_with_registry` still assumes `Vec<String>`.

- [ ] **Step 3: Wire command model to runner**

In `execute_sandbox_command_with_registry`, remove the split of `command` into `program,args` and replace runtime PATH executable resolution with:

```rust
let argv = runner::command_argv_from_sandbox_command(&command)?;
...
runner::execute(PlatformExecuteRequest {
    command: argv,
    cwd,
    env,
    timeout,
    policy,
})
.await
```

Keep `merge_path_entries(&mut env, &exec_env.path_entries)` exactly before extending with request env so registered runtime bins still appear first in `PATH`.

- [ ] **Step 4: Run tests and verify GREEN**

Run:

```bash
cargo test -p ahandd public_api::tests::execute_sandbox_command_rejects_empty_command
cargo test -p ahandd public_api::tests::execute_sandbox_command_accepts_shell_command_model
```

Expected: PASS.

### Task 4: Platform Runner And Smoke Compatibility

**Files:**
- Modify: `crates/ahandd/src/sandbox/platform/macos.rs`
- Modify: `crates/ahandd/src/sandbox/platform/windows.rs`
- Modify: `crates/ahandd/tests/sandbox_smoke.rs`

- [ ] **Step 1: Write failing macOS argv test**

Update the existing `sandbox_exec_argv_separates_policy_from_sandboxed_command` test to call:

```rust
let argv = sandbox_exec_args(
    "(version 1)".to_string(),
    &["/bin/sh".to_string(), "-c".to_string(), "echo ok".to_string()],
);
```

Assert:

```rust
assert_eq!(argv[0], "-p");
assert_eq!(argv[1], "(version 1)");
assert_eq!(argv[2], "--");
assert_eq!(argv[3], "/bin/sh");
assert_eq!(argv[4], "-c");
assert_eq!(argv[5], "echo ok");
```

Update the ignored `macos_runtime_denies_outside_read` test to build `PlatformExecuteRequest { command: vec![...] }`.

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test -p ahandd sandbox::platform::macos::tests::sandbox_exec_argv_separates_policy_from_sandboxed_command
```

Expected: FAIL because `sandbox_exec_args` still expects an executable plus args.

- [ ] **Step 3: Implement platform request changes**

In `macos.rs`, change:

```rust
let args = sandbox_exec_args(policy, &request.command);
```

and:

```rust
fn sandbox_exec_args(policy: String, command: &[String]) -> Vec<OsString> {
    let mut argv = vec![OsString::from("-p"), OsString::from(policy), OsString::from("--")];
    argv.extend(command.iter().map(OsString::from));
    argv
}
```

Add common user-installed executable roots to both root lists:

```rust
"/opt/homebrew",
"/usr/local",
```

Use `SandboxError::invalid_command("sandbox command must not be empty")` if `request.command` is empty before spawning `sandbox-exec`.

In `windows.rs`, no execution support is added in this task; only keep the new request shape compiling by reading `request.policy`.

In `sandbox_smoke.rs`, update existing direct argv requests:

```rust
command: SandboxCommand::Argv {
    command: vec!["python".to_string(), "-c".to_string(), "...".to_string()],
},
```

- [ ] **Step 4: Run tests and verify GREEN**

Run:

```bash
cargo test -p ahandd sandbox::platform::macos::tests::sandbox_exec_argv_separates_policy_from_sandboxed_command
cargo test -p ahandd --test sandbox_smoke
```

Expected: PASS on macOS with registered python/node runtimes available; if the smoke test cannot find a runtime, record the exact missing runtime error and continue with unit/integration tests.

### Task 5: aHand Verification And Commit

**Files:**
- All modified aHand files from Tasks 1-4

- [ ] **Step 1: Run focused test suite**

Run:

```bash
cargo test -p ahandd sandbox::types
cargo test -p ahandd sandbox::tool_provider
cargo test -p ahandd sandbox::runner
cargo test -p ahandd public_api::tests::execute_sandbox_command_rejects_empty_command
cargo test -p ahandd public_api::tests::execute_sandbox_command_accepts_shell_command_model
```

Expected: PASS.

- [ ] **Step 2: Run formatting and diff checks**

Run:

```bash
cargo fmt --check
git diff --check
git status --short
```

Expected: formatting passes, diff check passes, status shows only intended files under `crates/ahandd` and docs.

- [ ] **Step 3: Commit aHand implementation**

Run:

```bash
git add crates/ahandd/src/sandbox/types.rs crates/ahandd/src/sandbox/mod.rs crates/ahandd/src/sandbox/tool_provider.rs crates/ahandd/src/sandbox/runner.rs crates/ahandd/src/public_api.rs crates/ahandd/src/sandbox/platform/macos.rs crates/ahandd/src/sandbox/platform/windows.rs crates/ahandd/tests/sandbox_smoke.rs docs/superpowers/plans/2026-07-01-sandbox-run-command-shell-semantics.md
git commit -m "feat(ahandd): support shell run_command semantics"
```

Expected: commit succeeds and the new commit hash is available for the Coffice dependency pin.

### Task 6: Coffice Pin And Local End-To-End Verification

**Files:**
- Modify: `apps/desktop/src-tauri/Cargo.toml`
- Modify: `apps/desktop/src-tauri/Cargo.lock`

- [ ] **Step 1: Confirm Coffice Cargo files are clean before pinning**

Run:

```bash
git -C /Users/winrey/Projects/weightwave/Coffice status --short apps/desktop/src-tauri/Cargo.toml apps/desktop/src-tauri/Cargo.lock
```

Expected: no output. If either file is already modified, inspect the diff before changing it and preserve unrelated user edits.

- [ ] **Step 2: Update the aHand git rev**

In `apps/desktop/src-tauri/Cargo.toml`, change only the `ahandd` dependency `rev` to the aHand implementation commit hash from Task 5.

Run:

```bash
cargo update --manifest-path apps/desktop/src-tauri/Cargo.toml -p ahandd
```

Expected: `apps/desktop/src-tauri/Cargo.lock` records the same new git hash.

- [ ] **Step 3: Run Coffice Rust checks**

Run:

```bash
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml ahand
```

Expected: PASS. If unrelated existing Coffice worktree changes break this, capture the exact failing test name and error.

- [ ] **Step 4: Run local Coffice end-to-end sandbox checks**

Start or reuse the local desktop flow that exposes the sandbox tool provider, then invoke:

```json
{"tool":"run_command","args":{"cmd":"echo coffice-shell-ok"}}
```

Expected tool result:

```json
{
  "stdout": "coffice-shell-ok\n",
  "stderr": "",
  "exitCode": 0,
  "timedOut": false
}
```

Also invoke the compatibility path:

```json
{"tool":"run_command","args":{"command":["node","-e","console.log('coffice-argv-ok')"]}}
```

Expected stdout contains `coffice-argv-ok`.

- [ ] **Step 5: Commit Coffice pin**

Run:

```bash
git add apps/desktop/src-tauri/Cargo.toml apps/desktop/src-tauri/Cargo.lock
git commit -m "chore: update ahand sandbox command support"
```

Expected: commit contains only the aHand dependency pin and lockfile update.

### Task 7: Push, PR, And Merge To dev

**Files:**
- Git metadata only

- [ ] **Step 1: Push aHand branch**

Run:

```bash
git -C /Users/winrey/Projects/weightwave/aHand/.worktrees/sandbox-tool-provider push origin codex/ahand-sandbox-tool-provider
```

Expected: push succeeds.

- [ ] **Step 2: Push Coffice branch**

Run:

```bash
git -C /Users/winrey/Projects/weightwave/Coffice push origin codex/coffice-use-ahand-sandbox-tools
```

Expected: push succeeds.

- [ ] **Step 3: Create or update PRs and merge to dev**

Use the repo's existing GitHub remote and `gh` authentication:

```bash
gh pr create --base dev --head codex/ahand-sandbox-tool-provider --title "Support shell run_command semantics" --body "Adds Codex-style cmd support to sandbox run_command while preserving legacy argv command compatibility."
```

For Coffice:

```bash
gh pr create --base dev --head codex/coffice-use-ahand-sandbox-tools --title "Update aHand sandbox command support" --body "Pins Coffice desktop to the aHand sandbox command implementation and verifies local run_command cmd and legacy argv paths."
```

If a PR already exists, use `gh pr view` and `gh pr edit` instead of creating duplicates. Merge to `dev` only after both PRs are green or after the repo allows admin merge with the recorded local verification evidence.
