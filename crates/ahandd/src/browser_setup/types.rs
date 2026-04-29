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
    /// An install step ran and failed. Only produced by the mutating
    /// (`run_all` / `run_step`) paths; `inspect_*` never returns this.
    Failed {
        code: ErrorCode,
        /// Full `anyhow::Error` stringification — for the log drawer.
        message: String,
    },
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<u8>,
    /// Set when `phase == Log`, absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<LogStream>,
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
    /// A raw log line from the running step. Check `ProgressEvent.stream`
    /// to disambiguate stdout / stderr / synthesized info messages.
    /// `message` carries the line content (no trailing newline);
    /// `percent` is always `None`.
    Log,
}

/// Which stream a log line originated from. `Info` is synthesized by
/// Rust code; `Stdout`/`Stderr` are forwarded verbatim from child
/// processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    Stdout,
    Stderr,
    Info,
}

/// Machine-readable classification of an install-step failure.
/// Attached to `CheckStatus::Failed` (and to the terminal
/// `ProgressEvent` for the failing step) so the UI can pick a
/// targeted help popover without pattern-matching English prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// npm / install path returned EACCES. Remedy: chown / sudo.
    PermissionDenied,
    /// npm / download could not reach the registry.
    Network,
    /// No Chrome / Edge detected on the system.
    NoSystemBrowser,
    /// Node.js / npm not on PATH. Remedy: run the node step first.
    NodeMissing,
    /// Installed version did not match the pinned playwright-cli
    /// version. Remedy: retry with `force=true`.
    VersionMismatch,
    /// Catch-all for unclassified install errors.
    Unknown,
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
            stream: None,
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

    #[test]
    fn progress_event_serializes_log_phase_with_stream() {
        let event = ProgressEvent {
            step: "playwright",
            phase: Phase::Log,
            message: "npm warn deprecated foo@1.2.3".into(),
            percent: None,
            stream: Some(LogStream::Stderr),
        };
        let actual = serde_json::to_value(&event).unwrap();
        assert_eq!(
            actual,
            json!({
                "step": "playwright",
                "phase": "log",
                "message": "npm warn deprecated foo@1.2.3",
                "stream": "stderr"
            })
        );
    }

    #[test]
    fn progress_event_omits_stream_when_none() {
        let event = ProgressEvent {
            step: "node",
            phase: Phase::Starting,
            message: "Starting Node install".into(),
            percent: None,
            stream: None,
        };
        let actual = serde_json::to_value(&event).unwrap();
        assert!(
            actual.as_object().unwrap().get("stream").is_none(),
            "stream field should be absent when None: {actual}"
        );
    }

    #[test]
    fn progress_event_omits_percent_when_none() {
        let event = ProgressEvent {
            step: "node",
            phase: Phase::Done,
            message: "".into(),
            percent: None,
            stream: None,
        };
        let actual = serde_json::to_value(&event).unwrap();
        assert!(
            actual.as_object().unwrap().get("percent").is_none(),
            "percent field should be absent when None: {actual}"
        );
    }

    #[test]
    fn log_stream_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(LogStream::Stdout).unwrap(),
            json!("stdout")
        );
        assert_eq!(
            serde_json::to_value(LogStream::Stderr).unwrap(),
            json!("stderr")
        );
        assert_eq!(
            serde_json::to_value(LogStream::Info).unwrap(),
            json!("info")
        );
    }

    #[test]
    fn error_code_serializes_each_variant() {
        let cases = [
            (ErrorCode::PermissionDenied, "permission_denied"),
            (ErrorCode::Network, "network"),
            (ErrorCode::NoSystemBrowser, "no_system_browser"),
            (ErrorCode::NodeMissing, "node_missing"),
            (ErrorCode::VersionMismatch, "version_mismatch"),
            (ErrorCode::Unknown, "unknown"),
        ];
        for (variant, expected) in cases {
            assert_eq!(
                serde_json::to_value(variant).unwrap(),
                json!(expected),
                "variant {variant:?} should serialize as {expected}"
            );
        }
    }

    #[test]
    fn check_status_failed_serializes_with_tag() {
        let status = CheckStatus::Failed {
            code: ErrorCode::PermissionDenied,
            message: "EACCES: /foo/bar".into(),
        };
        let actual = serde_json::to_value(&status).unwrap();
        assert_eq!(
            actual,
            json!({
                "kind": "failed",
                "code": "permission_denied",
                "message": "EACCES: /foo/bar"
            })
        );
    }
}
