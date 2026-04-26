use std::path::PathBuf;

use serde::Serialize;

/// Status of a single setup check.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckStatus {
    /// Component is installed and meets requirements.
    Ok {
        version: String,
        path: PathBuf,
        source: CheckSource,
    },
    /// Component is not installed.
    Missing,
    /// Component is installed but version is too old.
    Outdated {
        current: String,
        required: String,
        path: PathBuf,
    },
    /// Applies to the browser check: none of the known browsers were found.
    NoneDetected { tried: Vec<String> },
}

/// Where a detected component comes from.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckSource {
    /// Installed by ahandd under ~/.ahand/...
    Managed,
    /// System-wide install (e.g. Chrome under /Applications).
    System,
    /// OS-shipped default (e.g. Edge on Windows).
    Preinstalled,
}

/// Full report for a single check, including any fix hint.
#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    /// Internal name: "node", "playwright", "browser".
    pub name: &'static str,
    /// Human-readable label: "Node.js", "playwright-cli", "System Browser".
    pub label: &'static str,
    pub status: CheckStatus,
    pub fix_hint: Option<FixHint>,
}

/// How to fix a failed check.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FixHint {
    /// Run `ahandd browser-init --step <name>`.
    RunStep { command: String },
    /// Manual per-platform commands the user must run themselves.
    ManualCommand {
        platform_commands: Vec<PlatformCommand>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct PlatformCommand {
    pub platform: &'static str, // "macOS" / "Linux" / "Windows"
    pub command: String,
}

/// Progress update emitted during install operations.
#[derive(Debug, Clone, Serialize)]
pub struct ProgressEvent {
    /// Which step is reporting: "node" / "playwright".
    pub step: &'static str,
    pub phase: Phase,
    pub message: String,
    /// Percent complete (0-100), only populated for measurable operations.
    pub percent: Option<u8>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Starting,
    Downloading,
    Extracting,
    Installing,
    Verifying,
    Done,
}

/// A detected system browser.
#[derive(Debug, Clone, Serialize)]
pub struct DetectedBrowser {
    pub name: String,
    pub path: PathBuf,
    pub kind: BrowserKind,
    pub source: CheckSource,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserKind {
    Chrome,
    Chromium,
    Edge,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn check_status_ok_serializes_with_tag() {
        let status = CheckStatus::Ok {
            version: "24.13.0".into(),
            path: PathBuf::from("/foo/node"),
            source: CheckSource::Managed,
        };
        let actual = serde_json::to_value(&status).unwrap();
        assert_eq!(
            actual,
            json!({
                "kind": "ok",
                "version": "24.13.0",
                "path": "/foo/node",
                "source": "managed"
            })
        );
    }

    #[test]
    fn check_status_missing_serializes_with_tag_only() {
        let status = CheckStatus::Missing;
        let actual = serde_json::to_value(&status).unwrap();
        assert_eq!(actual, json!({ "kind": "missing" }));
    }

    #[test]
    fn fix_hint_run_step_serializes() {
        let hint = FixHint::RunStep {
            command: "ahandd browser-init --step node".into(),
        };
        let actual = serde_json::to_value(&hint).unwrap();
        assert_eq!(
            actual,
            json!({
                "kind": "run_step",
                "command": "ahandd browser-init --step node"
            })
        );
    }

    #[test]
    fn progress_event_serializes_with_snake_case_phase() {
        let event = ProgressEvent {
            step: "node",
            phase: Phase::Downloading,
            message: "Downloading tarball".into(),
            percent: Some(42),
        };
        let actual = serde_json::to_value(&event).unwrap();
        assert_eq!(
            actual,
            json!({
                "step": "node",
                "phase": "downloading",
                "message": "Downloading tarball",
                "percent": 42
            })
        );
    }

    #[test]
    fn browser_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(&BrowserKind::Edge).unwrap(),
            json!("edge")
        );
    }
}
