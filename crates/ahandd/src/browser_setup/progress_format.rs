//! Shared presentation helpers for `ProgressEvent` and `CheckReport`.
//!
//! Both the `ahandd` CLI and `ahandctl` delegate their human-readable output
//! here so the two surfaces stay in sync. The admin panel's SSE adapter also
//! calls `format_progress_line` for the per-line content before wrapping it
//! in the JSON envelope.

use super::types::{CheckReport, CheckStatus, LogStream, Phase, ProgressEvent};

/// Format a single `ProgressEvent` as a human-readable CLI line.
///
/// Rules:
/// - `Phase::Done`   → `✓ <message>`
/// - `Phase::Failed` → `✗ <message>`
/// - `Phase::Log` with `stream == Some(LogStream::Stderr)` → `[stderr] <message>`
/// - All other phases → `<message>` (no prefix)
pub fn format_progress_line(event: &ProgressEvent) -> String {
    match event.phase {
        Phase::Done => format!("\u{2713} {}", event.message),
        Phase::Failed => format!("\u{2717} {}", event.message),
        Phase::Log => match event.stream {
            Some(LogStream::Stderr) => format!("[stderr] {}", event.message),
            _ => event.message.clone(),
        },
        Phase::Starting
        | Phase::Downloading
        | Phase::Extracting
        | Phase::Installing
        | Phase::Verifying => event.message.clone(),
    }
}

/// Format a summary table for a slice of `CheckReport`s.
///
/// Returns one line per report, without a trailing newline on the last line.
pub fn format_summary(reports: &[CheckReport]) -> String {
    let lines: Vec<String> = reports.iter().map(format_report_line).collect();
    lines.join("\n")
}

fn format_report_line(report: &CheckReport) -> String {
    match &report.status {
        CheckStatus::Ok { version, path, .. } => {
            let version_str = if version.is_empty() {
                String::new()
            } else {
                format!(" {version}")
            };
            format!("  {}:{} ({})", report.label, version_str, path.display())
        }
        CheckStatus::Missing => format!("  {}: still missing", report.label),
        CheckStatus::Outdated {
            current, required, ..
        } => format!("  {}: {current} (need {required})", report.label),
        CheckStatus::NoneDetected { tried } => format!(
            "  {}: none detected (tried: {})",
            report.label,
            tried.join(", ")
        ),
        CheckStatus::Failed { code, message } => {
            format!("  {}: failed ({code:?}): {message}", report.label)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser_setup::types::{
        CheckSource, CheckStatus, ErrorCode, FixHint, Phase, ProgressEvent,
    };
    use std::path::PathBuf;

    fn make_event(phase: Phase, message: &str, stream: Option<LogStream>) -> ProgressEvent {
        ProgressEvent {
            step: "node",
            phase,
            message: message.to_string(),
            percent: None,
            stream,
        }
    }

    // ── format_progress_line ────────────────────────────────────────────

    #[test]
    fn done_phase_gets_check_mark() {
        let e = make_event(Phase::Done, "Node.js installed", None);
        assert_eq!(format_progress_line(&e), "\u{2713} Node.js installed");
    }

    #[test]
    fn failed_phase_gets_cross_mark() {
        let e = make_event(Phase::Failed, "EACCES: permission denied", None);
        assert_eq!(
            format_progress_line(&e),
            "\u{2717} EACCES: permission denied"
        );
    }

    #[test]
    fn log_stderr_gets_prefix() {
        let e = make_event(
            Phase::Log,
            "npm warn deprecated foo@1.0.0",
            Some(LogStream::Stderr),
        );
        assert_eq!(
            format_progress_line(&e),
            "[stderr] npm warn deprecated foo@1.0.0"
        );
    }

    #[test]
    fn log_stdout_no_prefix() {
        let e = make_event(Phase::Log, "npm notice flushed", Some(LogStream::Stdout));
        assert_eq!(format_progress_line(&e), "npm notice flushed");
    }

    #[test]
    fn log_info_no_prefix() {
        let e = make_event(
            Phase::Log,
            "<stdout read error: eof>",
            Some(LogStream::Info),
        );
        assert_eq!(format_progress_line(&e), "<stdout read error: eof>");
    }

    #[test]
    fn log_no_stream_no_prefix() {
        let e = make_event(Phase::Log, "some log line", None);
        assert_eq!(format_progress_line(&e), "some log line");
    }

    #[test]
    fn plain_phases_pass_message_through() {
        for phase in [
            Phase::Starting,
            Phase::Downloading,
            Phase::Extracting,
            Phase::Installing,
            Phase::Verifying,
        ] {
            let e = make_event(phase, "some message", None);
            assert_eq!(format_progress_line(&e), "some message");
        }
    }

    // ── format_summary ──────────────────────────────────────────────────

    fn ok_report() -> CheckReport {
        CheckReport {
            name: "node",
            label: "Node.js",
            status: CheckStatus::Ok {
                version: "v24.13.0".into(),
                path: PathBuf::from("/home/.ahand/node/bin/node"),
                source: CheckSource::Managed,
            },
            fix_hint: None,
        }
    }

    fn missing_report() -> CheckReport {
        CheckReport {
            name: "playwright",
            label: "playwright-cli",
            status: CheckStatus::Missing,
            fix_hint: Some(FixHint::RunStep {
                command: "ahandd browser-init --step playwright".into(),
            }),
        }
    }

    fn failed_report() -> CheckReport {
        CheckReport {
            name: "node",
            label: "Node.js",
            status: CheckStatus::Failed {
                code: ErrorCode::Network,
                message: "ECONNRESET".into(),
            },
            fix_hint: None,
        }
    }

    fn none_detected_report() -> CheckReport {
        CheckReport {
            name: "browser",
            label: "System Browser",
            status: CheckStatus::NoneDetected {
                tried: vec!["chrome".into(), "edge".into()],
            },
            fix_hint: None,
        }
    }

    fn outdated_report() -> CheckReport {
        CheckReport {
            name: "node",
            label: "Node.js",
            status: CheckStatus::Outdated {
                current: "v18.0.0".into(),
                required: "v20".into(),
                path: PathBuf::from("/usr/bin/node"),
            },
            fix_hint: None,
        }
    }

    #[test]
    fn summary_ok_report_shows_version_and_path() {
        let s = format_summary(&[ok_report()]);
        assert!(s.contains("Node.js"), "label must appear");
        assert!(s.contains("v24.13.0"), "version must appear");
        assert!(s.contains("node/bin/node"), "path must appear");
    }

    #[test]
    fn summary_missing_report_says_still_missing() {
        let s = format_summary(&[missing_report()]);
        assert!(s.contains("still missing"));
    }

    #[test]
    fn summary_failed_report_includes_code_and_message() {
        let s = format_summary(&[failed_report()]);
        assert!(s.contains("failed"), "must say 'failed'");
        assert!(s.contains("ECONNRESET"), "must include message");
    }

    #[test]
    fn summary_none_detected_lists_tried() {
        let s = format_summary(&[none_detected_report()]);
        assert!(s.contains("none detected"));
        assert!(s.contains("chrome"));
        assert!(s.contains("edge"));
    }

    #[test]
    fn summary_outdated_shows_current_and_required() {
        let s = format_summary(&[outdated_report()]);
        assert!(s.contains("v18.0.0"), "current version must appear");
        assert!(s.contains("v20"), "required version must appear");
    }

    #[test]
    fn summary_multiple_reports_are_newline_separated() {
        let s = format_summary(&[ok_report(), missing_report()]);
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines.len(), 2, "two reports → two lines");
    }

    #[test]
    fn summary_ok_report_empty_version_no_extra_space() {
        let report = CheckReport {
            name: "browser",
            label: "System Browser",
            status: CheckStatus::Ok {
                version: String::new(), // browser has no cheap version
                path: PathBuf::from("/Applications/Google Chrome.app"),
                source: CheckSource::System,
            },
            fix_hint: None,
        };
        let s = format_summary(&[report]);
        // Should NOT have ": " (colon-space-space) from empty version
        assert!(
            !s.contains(":  "),
            "empty version should not produce double space: {s}"
        );
    }
}
