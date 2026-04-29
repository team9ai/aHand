use anyhow::Result;

use crate::browser_setup::{self, Phase, ProgressEvent};

/// Entry point for `ahandd browser-init [--force] [--step <name>]`.
pub async fn run(force: bool, step: Option<String>) -> Result<()> {
    let progress = make_progress_printer();

    match step.as_deref() {
        Some(name) => {
            let report = browser_setup::run_step(name, force, progress).await?;
            println!();
            println!("Step `{name}` complete.");
            print_summary(&[report]);
        }
        None => {
            let reports = browser_setup::run_all(force, progress).await?;
            println!();
            println!("Setup complete.");
            print_summary(&reports);
        }
    }
    Ok(())
}

fn make_progress_printer() -> impl Fn(ProgressEvent) + Send + Sync + 'static {
    |event: ProgressEvent| match event.phase {
        Phase::Done => println!("  \u{2713} {}", event.message),
        Phase::Starting
        | Phase::Downloading
        | Phase::Extracting
        | Phase::Installing
        | Phase::Verifying => {
            println!("  {}", event.message);
        }
        Phase::Log => {
            println!("  {}", event.message);
        }
    }
}

fn print_summary(reports: &[browser_setup::CheckReport]) {
    use browser_setup::CheckStatus;
    for report in reports {
        match &report.status {
            CheckStatus::Ok { version, path, .. } => {
                let version_str = if version.is_empty() {
                    String::new()
                } else {
                    format!(" {version}")
                };
                println!("  {}:{} ({})", report.label, version_str, path.display());
            }
            CheckStatus::Missing => {
                println!("  {}: still missing", report.label);
            }
            CheckStatus::Outdated {
                current, required, ..
            } => {
                println!("  {}: {current} (need {required})", report.label);
            }
            CheckStatus::NoneDetected { tried } => {
                println!(
                    "  {}: none detected (tried: {})",
                    report.label,
                    tried.join(", ")
                );
            }
            CheckStatus::Failed { code, message } => {
                println!("  {}: failed ({code:?}): {message}", report.label);
            }
        }
    }
}
