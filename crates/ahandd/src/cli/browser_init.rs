use anyhow::Result;

use crate::browser_setup::{self, ProgressEvent, format_progress_line, format_summary};

/// Entry point for `ahandd browser-init [--force] [--step <name>]`.
pub async fn run(force: bool, step: Option<String>) -> Result<()> {
    let progress = make_progress_printer();

    match step.as_deref() {
        Some(name) => {
            let report = browser_setup::run_step(name, force, progress).await?;
            println!();
            println!("Plugin step `{name}` complete.");
            println!("{}", format_summary(&[report]));
        }
        None => {
            let reports = browser_setup::run_all(force, progress).await?;
            println!();
            println!("Setup complete.");
            println!("{}", format_summary(&reports));
        }
    }
    Ok(())
}

fn make_progress_printer() -> impl Fn(ProgressEvent) + Send + Sync + 'static {
    |event: ProgressEvent| {
        println!("  {}", format_progress_line(&event));
    }
}
