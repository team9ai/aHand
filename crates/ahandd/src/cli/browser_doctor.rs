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
    match hint {
        FixHint::RunStep { command } => {
            println!("  {label}  \u{2192}  {command}");
        }
        FixHint::ManualCommand { platform_commands } => {
            println!("  {label}:");
            for PlatformCommand { platform, command } in platform_commands {
                println!("    {platform:<8}  {command}");
            }
        }
    }
}
