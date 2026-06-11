use anyhow::Result;

use ahandd::browser_setup::{self, Phase, ProgressEvent};

/// Print a `ProgressEvent` to stdout in the same human-readable style that
/// `setup-browser.sh` produced: step lines on `stdout`, done lines prefixed
/// with a check-mark.
fn print_progress(event: ProgressEvent) {
    match event.phase {
        Phase::Done => println!("  \u{2713} {}", event.message),
        Phase::Starting
        | Phase::Downloading
        | Phase::Extracting
        | Phase::Installing
        | Phase::Verifying => {
            println!("  {}", event.message);
        }
        // Log lines (npm output, etc.) — print verbatim.
        Phase::Log => {
            println!("  {}", event.message);
        }
    }
}

/// Entry point for `ahandctl browser-init [--force]`.
///
/// Replaces the old bash spawn of `setup-browser.sh`: calls the canonical
/// `ahandd::browser_setup::run_all` orchestration directly (no shell, no
/// script file on disk required).  `--force` maps to the existing
/// `force=true` semantics in the library (reinstalls even if already present).
pub async fn run(force: bool) -> Result<()> {
    println!("Running browser setup...");
    println!();

    if force {
        println!("Force mode: will reinstall even if already present.");
        println!();
    }

    let reports = browser_setup::run_all(force, print_progress).await?;

    println!();
    println!("Setup complete.");

    use browser_setup::CheckStatus;
    for report in &reports {
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

    Ok(())
}
