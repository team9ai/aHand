use anyhow::Result;

use ahandd::browser_setup::{self, ProgressEvent, format_progress_line, format_summary};

/// Print a `ProgressEvent` to stdout in the same human-readable style that
/// `setup-browser.sh` produced: step lines on `stdout`, done lines prefixed
/// with a check-mark, failure lines prefixed with `✗`.
fn print_progress(event: ProgressEvent) {
    println!("  {}", format_progress_line(&event));
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
    println!("{}", format_summary(&reports));

    Ok(())
}
