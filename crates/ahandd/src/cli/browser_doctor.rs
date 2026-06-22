use anyhow::Result;

use crate::browser_setup::{self, CheckReport, CheckStatus, FixHint, PlatformCommand};

/// Entry point for `ahandd browser-doctor`.
pub async fn run() -> Result<()> {
    println!("Browser Automation Diagnostics");
    println!("==============================");

    let reports = browser_setup::inspect_all().await;
    for report in &reports {
        print_check(report);
    }
    println!();

    let failures: Vec<&CheckReport> = reports
        .iter()
        .filter(|r| !matches!(r.status, CheckStatus::Ok { .. }))
        .collect();

    if failures.is_empty() {
        println!("Status: all checks passed.");
        return Ok(());
    }

    println!("Status: {} issue(s) found.", failures.len());
    println!();
    println!("Fix suggestions:");
    for failure in &failures {
        if let Some(hint) = &failure.fix_hint {
            print_fix_hint(failure.label, hint);
        }
    }

    std::process::exit(1);
}

fn print_check(report: &CheckReport) {
    let (marker, line) = match &report.status {
        CheckStatus::Ok {
            version,
            path,
            source,
        } => {
            let suffix = match source {
                crate::browser_setup::CheckSource::Managed => String::new(),
                crate::browser_setup::CheckSource::System => " (system)".into(),
                crate::browser_setup::CheckSource::Preinstalled => " (preinstalled)".into(),
            };
            let version_str = if version.is_empty() {
                String::new()
            } else {
                format!("{version}  ")
            };
            (
                "[\u{2713}]",
                format!(
                    "{:<17} {version_str}({}){suffix}",
                    format!("{}:", report.label),
                    path.display()
                ),
            )
        }
        CheckStatus::Missing => (
            "[\u{2717}]",
            format!("{:<17} not found", format!("{}:", report.label)),
        ),
        CheckStatus::Outdated {
            current,
            required,
            path,
        } => (
            "[\u{2717}]",
            format!(
                "{:<17} {current} (need {required}) at {}",
                format!("{}:", report.label),
                path.display()
            ),
        ),
        CheckStatus::NoneDetected { tried } => (
            "[\u{2717}]",
            format!(
                "{:<17} none detected\n                     Tried: {}",
                format!("{}:", report.label),
                tried.join(", ")
            ),
        ),
        // TODO(task-2): proper failure rendering with ErrorCode-based hints; stub is debug-only
        CheckStatus::Failed { code, message } => (
            "[\u{2717}]",
            format!(
                "{:<17} failed ({code:?}): {message}",
                format!("{}:", report.label)
            ),
        ),
    };
    println!("{marker} {line}");
}

fn print_fix_hint(label: &str, hint: &FixHint) {
    print!("{}", format_fix_hint(label, hint));
}

/// Display label of the host platform, matching the exact vocabulary used by
/// `PlatformCommand::platform` (`"macOS"` / `"Linux"` / `"Windows"`).
///
/// Uses `cfg!(target_os = ...)` rather than `std::env::consts::OS` on purpose:
/// the latter yields lowercase (`"macos"`/`"linux"`/`"windows"`) which would
/// match NONE of the capitalized labels and silently hide every hint. Returns
/// `None` on an OS without a known label so callers can fall back to showing
/// all platforms.
fn host_platform_label() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("macOS")
    } else if cfg!(target_os = "linux") {
        Some("Linux")
    } else if cfg!(target_os = "windows") {
        Some("Windows")
    } else {
        None
    }
}

/// Render a fix hint for the human-facing CLI.
///
/// For `ManualCommand`, only the host platform's command line is shown (a Linux
/// user has no use for the macOS `brew` command). If the host platform has no
/// matching entry — an unknown OS, or a label that isn't in the list — all
/// entries are printed so the user is never left with no remediation at all.
///
/// This filters ONLY the printed text; the serialized `FixHint` (consumed by
/// `/api/status` and the admin surfaces) still carries every platform.
fn format_fix_hint(label: &str, hint: &FixHint) -> String {
    match hint {
        FixHint::RunStep { command } => {
            format!("  {label}  \u{2192}  {command}\n")
        }
        FixHint::ManualCommand { platform_commands } => {
            let host = host_platform_label();
            // Show only the host platform's line when it has a matching entry;
            // otherwise fall back to all entries so we never print nothing.
            let host_matched =
                host.is_some_and(|h| platform_commands.iter().any(|pc| pc.platform == h));

            let mut out = format!("  {label}:\n");
            for PlatformCommand { platform, command } in platform_commands {
                if host_matched && Some(*platform) != host {
                    continue;
                }
                out.push_str(&format!("    {platform:<8}  {command}\n"));
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_command_shows_only_host_platform() {
        let hint = FixHint::ManualCommand {
            platform_commands: vec![
                PlatformCommand {
                    platform: "macOS",
                    command: "brew install --cask google-chrome".into(),
                },
                PlatformCommand {
                    platform: "Linux",
                    command: "sudo apt install chromium-browser".into(),
                },
                PlatformCommand {
                    platform: "Windows",
                    command: "Edge should be preinstalled".into(),
                },
            ],
        };

        let rendered = format_fix_hint("System Browser", &hint);

        // The host label resolves via the same cfg! helper the renderer uses,
        // so this assertion holds on macOS, Linux, and Windows CI alike.
        let host = host_platform_label().expect("test target should have a known host label");

        // Exactly one command line (lines indented with four spaces).
        let command_lines: Vec<&str> = rendered.lines().filter(|l| l.starts_with("    ")).collect();
        assert_eq!(
            command_lines.len(),
            1,
            "exactly one command line expected, got: {rendered:?}"
        );
        assert!(
            command_lines[0].contains(host),
            "the sole command line should be the host platform `{host}`: {rendered:?}"
        );

        // The two other-OS commands must be absent.
        for (other_label, marker) in [
            ("macOS", "brew install"),
            ("Linux", "apt install"),
            ("Windows", "preinstalled"),
        ] {
            if other_label != host {
                assert!(
                    !rendered.contains(marker),
                    "other-OS hint `{marker}` ({other_label}) should be filtered out: {rendered:?}"
                );
            }
        }
    }

    #[test]
    fn manual_command_falls_back_to_all_when_host_unmatched() {
        // No entry matches the host label -> print everything, never nothing.
        let hint = FixHint::ManualCommand {
            platform_commands: vec![PlatformCommand {
                platform: "Plan9",
                command: "pkg_add chrome".into(),
            }],
        };
        let rendered = format_fix_hint("System Browser", &hint);
        assert!(
            rendered.contains("Plan9") && rendered.contains("pkg_add chrome"),
            "unmatched host must fall back to all entries: {rendered:?}"
        );
    }

    #[test]
    fn run_step_hint_renders_command() {
        let hint = FixHint::RunStep {
            command: "ahandd browser-init --step node".into(),
        };
        let rendered = format_fix_hint("Node.js", &hint);
        assert!(rendered.contains("ahandd browser-init --step node"));
    }
}
