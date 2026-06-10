//! Pact: SDK `SpawnParams.tool` ↔ ahandd `executor::resolve_tool`.
//!
//! The TS SDK and the Rust daemon agreed on a small alphabet of `tool`
//! tokens that travel over the wire as `JobRequest.tool` (proto field
//! `string tool = 2`). Today's contract drift was exactly this surface —
//! `"shell"` literal vs `"$SHELL"` — and the unit tests inside the daemon
//! crate (`mod tool_resolution_tests`) caught it only because someone
//! happened to look. This file makes the same checks **from the outside**,
//! through the daemon's published library API, so an SDK author who clones
//! the repo and runs `cargo test -p ahandd --test job_request_tool` sees
//! exactly the contract their `SpawnParams.tool` value will be measured
//! against.
//!
//! When the SDK adds a new `tool` token (e.g. `"powershell"`), bump the
//! cases below and add a matching arm in `executor::resolve_tool` —
//! ideally in the same PR.

use ahand_platform::shell::default_shell;
use ahandd::executor::{ResolvedTool, resolve_tool};

#[derive(Debug)]
struct Case {
    /// What the SDK sets `JobRequest.tool` to.
    tool: &'static str,
    /// What the daemon sees as `$SHELL` at exec time.
    shell_env: Option<&'static str>,
    /// What the daemon must resolve to. `path` becomes the argv[0]
    /// passed to `Command::new`; `leading_args` are spliced in front of
    /// the user-supplied args.
    expected: ResolvedTool,
    /// Why this case is in the table — keeps the diff legible when the
    /// alphabet changes.
    rationale: &'static str,
}

fn cases() -> Vec<Case> {
    let shell = default_shell();
    vec![
        Case {
            tool: "$SHELL",
            shell_env: Some("/bin/zsh"),
            expected: ResolvedTool {
                path: "/bin/zsh".into(),
                leading_args: shell.login_args.clone(),
            },
            rationale: "canonical sentinel — SDK's recommended way to ask for the user's login shell",
        },
        Case {
            tool: "shell",
            shell_env: Some("/bin/bash"),
            expected: ResolvedTool {
                path: "/bin/bash".into(),
                leading_args: shell.login_args.clone(),
            },
            rationale: "older SDK callers emit the bare word `shell` — both must keep working",
        },
        Case {
            tool: "$SHELL",
            shell_env: None,
            expected: ResolvedTool {
                path: shell.path.clone(),
                leading_args: shell.login_args.clone(),
            },
            rationale: "$SHELL unset (e.g. launchd) must still produce a runnable command, not ENOENT",
        },
        Case {
            tool: "shell",
            shell_env: None,
            expected: ResolvedTool {
                path: shell.path.clone(),
                leading_args: shell.login_args.clone(),
            },
            rationale: "same fallback path as $SHELL — sentinel parity",
        },
        Case {
            tool: "/bin/sh",
            shell_env: Some("/bin/zsh"),
            expected: ResolvedTool {
                path: "/bin/sh".into(),
                leading_args: vec![],
            },
            rationale: "`/bin/sh` is a literal binary path, NOT a sentinel — \
                        catches the regression where a hardcoded `/bin/sh` from \
                        claw-hive accidentally activates login-shell mode",
        },
        Case {
            tool: "/usr/bin/whoami",
            shell_env: Some("/bin/zsh"),
            expected: ResolvedTool {
                path: "/usr/bin/whoami".into(),
                leading_args: vec![],
            },
            rationale: "absolute path → pass through with no leading args",
        },
        Case {
            tool: "git",
            shell_env: Some("/bin/zsh"),
            expected: ResolvedTool {
                path: "git".into(),
                leading_args: vec![],
            },
            rationale: "PATH-resolvable binary name → pass through with no leading args",
        },
    ]
}

#[test]
fn sdk_to_daemon_tool_resolution_is_table_complete() {
    let cases = cases();
    assert!(
        !cases.is_empty(),
        "table must have at least one case — \
         empty test is a silent contract regression"
    );

    let mut failures: Vec<String> = Vec::new();
    for (idx, case) in cases.iter().enumerate() {
        let actual = resolve_tool(case.tool, case.shell_env);
        if actual != case.expected {
            failures.push(format!(
                "  case #{idx} (tool={:?}, $SHELL={:?})\n    \
                 rationale: {}\n    \
                 expected:  {:?}\n    \
                 actual:    {:?}",
                case.tool, case.shell_env, case.rationale, case.expected, actual,
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "SDK→daemon tool resolution drift detected:\n{}",
        failures.join("\n"),
    );
}

#[test]
fn sentinels_always_inject_platform_login_args() {
    // Independent invariant: every sentinel form must emit the platform
    // login args (`-l` on Unix; empty on Windows by design). If a future
    // refactor drops them for one variant but not the other, spawned
    // commands silently lose their PATH (no brew/nvm/pyenv shims on Unix)
    // — exactly the failure mode that today's debugging chased down.
    let expected_login_args = default_shell().login_args;
    for sentinel in ["$SHELL", "shell"] {
        for shell_env in [Some("/bin/zsh"), Some("/bin/bash"), None] {
            let r = resolve_tool(sentinel, shell_env);
            assert_eq!(
                r.leading_args, expected_login_args,
                "sentinel {sentinel:?} with $SHELL={shell_env:?} produced unexpected \
                 leading_args; expected platform login args {:?}",
                expected_login_args,
            );
        }
    }
}

#[test]
fn non_sentinel_tools_never_inject_leading_args() {
    // Mirror invariant: any non-sentinel value must pass through with
    // an empty `leading_args`. If we ever start prepending args for
    // arbitrary tools, the SDK's understanding of how `JobRequest.args`
    // ends up on argv silently diverges from reality.
    let pass_through = [
        "/bin/sh",
        "/bin/bash",
        "/usr/bin/whoami",
        "git",
        "rg",
        "node",
        "python3",
    ];
    for tool in pass_through {
        let r = resolve_tool(tool, Some("/bin/zsh"));
        assert!(
            r.leading_args.is_empty(),
            "non-sentinel tool {tool:?} unexpectedly produced leading_args={:?}",
            r.leading_args
        );
        assert_eq!(
            r.path, tool,
            "non-sentinel tool {tool:?} must pass through unchanged; got path={:?}",
            r.path
        );
    }
}
