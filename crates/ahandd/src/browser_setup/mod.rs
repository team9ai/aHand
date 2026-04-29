//! Browser automation setup: checks, installs, and browser detection.
//!
//! Public API:
//! - `inspect_all()`, `inspect(name)` — read-only diagnostic
//! - `run_all(force, progress)` — install everything (or refresh)
//! - `run_step(name, force, progress)` — install a single component
//! - `detect_browser(config_override)`, `detect_all_browsers()` — browser detection

use anyhow::{Result, bail};

pub mod browser_detect;
pub mod node;
pub mod playwright;
pub mod types;

pub use browser_detect::{
    detect as detect_browser, detect_all as detect_all_browsers, tried_browsers,
};
pub use types::*;

/// Classify an `anyhow::Error` produced by an install step into a
/// machine-readable `ErrorCode`. Patterns match the `bail!` call sites
/// in `playwright.rs` / `node.rs` and the `no system browser` message
/// from `inspect_browser`. The `{:#}` formatter walks the cause chain
/// so wrapped errors still classify by their root cause.
pub fn classify_error(err: &anyhow::Error) -> ErrorCode {
    let s = format!("{err:#}");
    if s.contains("Permission denied") || s.contains("EACCES") {
        ErrorCode::PermissionDenied
    } else if s.contains("Network error")
        || s.contains("ECONNRESET")
        || s.contains("ETIMEDOUT")
        || s.contains("getaddrinfo")
    {
        ErrorCode::Network
    } else if s.contains("no system browser") {
        ErrorCode::NoSystemBrowser
    } else if s.contains("npm not found") || (s.contains("Node") && s.contains("not installed")) {
        ErrorCode::NodeMissing
    } else if s.contains("version") && (s.contains("mismatch") || s.contains("required")) {
        ErrorCode::VersionMismatch
    } else {
        ErrorCode::Unknown
    }
}

/// Attached to `anyhow::Error` via `.context()` so callers (notably
/// team9's Tauri `browser_runtime`) can downcast and get the
/// classified `CheckReport` without re-parsing the error string.
///
/// Usage from the consumer side:
/// ```ignore
/// let err: anyhow::Error = /* returned from run_all */;
/// let failed: Option<&FailedStepReport> = err.downcast_ref::<FailedStepReport>();
/// if let Some(report) = failed {
///     // report.0 is the CheckReport
/// }
/// ```
pub struct FailedStepReport(pub CheckReport);

impl std::fmt::Display for FailedStepReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "step `{}` failed", self.0.name)
    }
}

impl std::fmt::Debug for FailedStepReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FailedStepReport({})", self.0.name)
    }
}

impl std::error::Error for FailedStepReport {}

/// Inspect all browser setup components. Read-only; never modifies anything.
pub async fn inspect_all() -> Vec<CheckReport> {
    vec![
        node::inspect().await,
        playwright::inspect().await,
        inspect_browser(),
    ]
}

/// Inspect a single component by name.
pub async fn inspect(name: &str) -> Option<CheckReport> {
    match name {
        "node" => Some(node::inspect().await),
        "playwright" => Some(playwright::inspect().await),
        "browser" => Some(inspect_browser()),
        _ => None,
    }
}

/// Run all install steps. `force` reinstalls even if already present.
pub async fn run_all(
    force: bool,
    progress: impl Fn(ProgressEvent) + Send + Sync + 'static,
) -> Result<Vec<CheckReport>> {
    let progress_ref: &(dyn Fn(ProgressEvent) + Send + Sync) = &progress;

    let node_report = match node::ensure(force, progress_ref).await {
        Ok(r) => r,
        Err(e) => return Err(wrap_failure(e, "node", "Node.js", progress_ref)),
    };

    let playwright_report = match playwright::ensure(force, progress_ref).await {
        Ok(r) => r,
        Err(e) => {
            return Err(wrap_failure(
                e,
                "playwright",
                "playwright-cli",
                progress_ref,
            ));
        }
    };

    let browser_report = inspect_browser();
    Ok(vec![node_report, playwright_report, browser_report])
}

/// Run a single install step. Valid names: `node`, `playwright`.
/// Returns an error for `playwright` if Node is not already installed.
pub async fn run_step(
    name: &str,
    force: bool,
    progress: impl Fn(ProgressEvent) + Send + Sync + 'static,
) -> Result<CheckReport> {
    let progress_ref: &(dyn Fn(ProgressEvent) + Send + Sync) = &progress;
    match name {
        "node" => match node::ensure(force, progress_ref).await {
            Ok(r) => Ok(r),
            Err(e) => Err(wrap_failure(e, "node", "Node.js", progress_ref)),
        },
        "playwright" => {
            let node_status = node::inspect().await;
            if !matches!(node_status.status, CheckStatus::Ok { .. }) {
                bail!(
                    "playwright step requires node to be installed first. \
                     Run `ahandd browser-init --step node` first, or \
                     `ahandd browser-init` for all steps."
                );
            }
            match playwright::ensure(force, progress_ref).await {
                Ok(r) => Ok(r),
                Err(e) => Err(wrap_failure(
                    e,
                    "playwright",
                    "playwright-cli",
                    progress_ref,
                )),
            }
        }
        other => bail!("unknown step `{other}`. Valid steps: node, playwright"),
    }
}

/// Build a `FailedStepReport`, emit a terminal `ProgressEvent::Done`, and
/// attach the report to the error via `.context(...)`. Called by
/// `run_all` / `run_step` on any `ensure()` failure.
fn wrap_failure(
    err: anyhow::Error,
    name: &'static str,
    label: &'static str,
    progress: &(dyn Fn(ProgressEvent) + Send + Sync),
) -> anyhow::Error {
    let code = classify_error(&err);
    let message = format!("{err:#}");
    progress(ProgressEvent {
        step: name,
        phase: Phase::Done,
        message: message.clone(),
        percent: None,
        stream: None,
    });
    let report = CheckReport {
        name,
        label,
        status: CheckStatus::Failed {
            code,
            message: message.clone(),
        },
        fix_hint: Some(FixHint::RunStep {
            command: format!("ahandd browser-init --step {name} --force"),
        }),
    };
    err.context(FailedStepReport(report))
}

fn inspect_browser() -> CheckReport {
    match detect_browser(None) {
        Some(browser) => CheckReport {
            name: "browser",
            label: "System Browser",
            status: CheckStatus::Ok {
                version: String::new(), // no cheap way to query version
                path: browser.path,
                source: browser.source,
            },
            fix_hint: None,
        },
        None => CheckReport {
            name: "browser",
            label: "System Browser",
            status: CheckStatus::NoneDetected {
                tried: tried_browsers(),
            },
            fix_hint: Some(FixHint::ManualCommand {
                platform_commands: vec![
                    PlatformCommand {
                        platform: "macOS",
                        command: "brew install --cask google-chrome".into(),
                    },
                    PlatformCommand {
                        platform: "Linux",
                        command: "sudo apt install chromium-browser (or microsoft-edge-stable)"
                            .into(),
                    },
                    PlatformCommand {
                        platform: "Windows",
                        command: "Edge should be preinstalled — please report".into(),
                    },
                ],
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_step_rejects_unknown_name() {
        let progress = |_: ProgressEvent| {};
        let result = run_step("unknown", false, progress).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown step"));
    }

    #[tokio::test]
    async fn inspect_all_returns_three_reports() {
        let reports = inspect_all().await;
        assert_eq!(reports.len(), 3);
        assert_eq!(reports[0].name, "node");
        assert_eq!(reports[1].name, "playwright");
        assert_eq!(reports[2].name, "browser");
    }

    #[tokio::test]
    async fn inspect_by_name() {
        assert!(inspect("node").await.is_some());
        assert!(inspect("playwright").await.is_some());
        assert!(inspect("browser").await.is_some());
        assert!(inspect("nothing").await.is_none());
    }

    #[test]
    fn classify_error_permission_denied() {
        let err = anyhow::anyhow!("EACCES: permission denied at /foo");
        assert_eq!(classify_error(&err), ErrorCode::PermissionDenied);

        let err2 = anyhow::anyhow!("Permission denied writing to /bar");
        assert_eq!(classify_error(&err2), ErrorCode::PermissionDenied);
    }

    #[test]
    fn classify_error_network() {
        for msg in [
            "Network error while downloading",
            "ECONNRESET from registry",
            "ETIMEDOUT waiting for response",
            "getaddrinfo ENOTFOUND registry.npmjs.org",
        ] {
            let err = anyhow::anyhow!("{msg}");
            assert_eq!(
                classify_error(&err),
                ErrorCode::Network,
                "msg `{msg}` should classify as Network",
            );
        }
    }

    #[test]
    fn classify_error_no_system_browser() {
        let err = anyhow::anyhow!("no system browser (Chrome/Edge) detected — please install one");
        assert_eq!(classify_error(&err), ErrorCode::NoSystemBrowser);
    }

    #[test]
    fn classify_error_node_missing() {
        let err = anyhow::anyhow!("npm not found at /usr/local/bin/npm");
        assert_eq!(classify_error(&err), ErrorCode::NodeMissing);

        let err2 = anyhow::anyhow!("Node is not installed");
        assert_eq!(classify_error(&err2), ErrorCode::NodeMissing);
    }

    #[test]
    fn classify_error_version_mismatch() {
        let err = anyhow::anyhow!("version mismatch: got 0.1.0, required 0.1.1");
        assert_eq!(classify_error(&err), ErrorCode::VersionMismatch);

        let err2 = anyhow::anyhow!("version required: 0.1.1");
        assert_eq!(classify_error(&err2), ErrorCode::VersionMismatch);
    }

    #[test]
    fn classify_error_unknown_fallback() {
        let err = anyhow::anyhow!("some unrecognized failure");
        assert_eq!(classify_error(&err), ErrorCode::Unknown);
    }

    #[test]
    fn classify_error_walks_cause_chain() {
        let root = anyhow::anyhow!("EACCES: lower-level io");
        let wrapped = root.context("npm install failed");
        assert_eq!(
            classify_error(&wrapped),
            ErrorCode::PermissionDenied,
            "classifier must see the root cause via `{{:#}}`",
        );
    }

    #[tokio::test]
    async fn failed_step_report_attached_to_error() {
        // Contrive a run_step failure by hitting the "unknown step" bail —
        // but that doesn't go through wrap_failure. Instead, run_step("node", ...)
        // with a forced failure isn't easy to mock without refactoring node::ensure.
        //
        // This test is deliberately narrow: use the public classifier + newtype
        // round-trip, since the full end-to-end "ensure fails → report attached"
        // path is covered by the integration tests in `tests/browser_setup.rs`
        // (see Task 7 CI run).
        let report = CheckReport {
            name: "node",
            label: "Node.js",
            status: CheckStatus::Failed {
                code: ErrorCode::PermissionDenied,
                message: "EACCES".into(),
            },
            fix_hint: None,
        };
        let err = anyhow::anyhow!("EACCES").context(FailedStepReport(report));
        // anyhow::Error::downcast_ref::<C>() handles context types — it
        // searches through ContextError<C, E> wrappers via type-id vtable.
        // The `chain().find_map(|e| e.downcast_ref::<C>())` pattern is for
        // concrete StdError types in the chain, not for context wrappers.
        let downcast = err.downcast_ref::<FailedStepReport>();
        assert!(
            downcast.is_some(),
            "FailedStepReport should be accessible via downcast_ref"
        );
        let failed = downcast.unwrap();
        assert_eq!(failed.0.name, "node");
        assert!(matches!(
            failed.0.status,
            CheckStatus::Failed {
                code: ErrorCode::PermissionDenied,
                ..
            }
        ));
    }

    #[test]
    fn failed_step_report_display_is_useful() {
        let report = CheckReport {
            name: "playwright",
            label: "playwright-cli",
            status: CheckStatus::Failed {
                code: ErrorCode::Network,
                message: "ECONNRESET".into(),
            },
            fix_hint: None,
        };
        let wrapper = FailedStepReport(report);
        assert_eq!(format!("{wrapper}"), "step `playwright` failed");
        assert_eq!(format!("{wrapper:?}"), "FailedStepReport(playwright)");
    }
}
