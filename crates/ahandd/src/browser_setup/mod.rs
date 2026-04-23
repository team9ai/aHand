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
    let node_report = node::ensure(force, progress_ref).await?;
    let playwright_report = playwright::ensure(force, progress_ref).await?;
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
        "node" => node::ensure(force, progress_ref).await,
        "playwright" => {
            let node_status = node::inspect().await;
            if !matches!(node_status.status, CheckStatus::Ok { .. }) {
                bail!(
                    "playwright step requires node to be installed first. \
                     Run `ahandd browser-init --step node` first, or \
                     `ahandd browser-init` for all steps."
                );
            }
            playwright::ensure(force, progress_ref).await
        }
        other => bail!("unknown step `{other}`. Valid steps: node, playwright"),
    }
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
}
