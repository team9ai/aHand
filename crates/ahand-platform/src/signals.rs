//! Unified shutdown signal: SIGTERM/SIGINT on Unix, Ctrl-C (incl. console
//! close) on Windows. Returns the *name* of the signal that fired, for logs.

use anyhow::Result;
use std::future::Future;

#[cfg(unix)]
pub fn shutdown_signal() -> Result<impl Future<Output = &'static str>> {
    use anyhow::Context as _;
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    let mut int = signal(SignalKind::interrupt()).context("install SIGINT handler")?;
    Ok(async move {
        tokio::select! {
            _ = term.recv() => "SIGTERM",
            _ = int.recv() => "SIGINT",
        }
    })
}

#[cfg(windows)]
pub fn shutdown_signal() -> Result<impl Future<Output = &'static str>> {
    Ok(async {
        // ctrl_c covers Ctrl-C / Ctrl-Break delivery for console processes.
        let _ = tokio::signal::ctrl_c().await;
        "ctrl-c"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_signal_is_ok_without_awaiting() {
        // Verify that installing the signal handlers succeeds in a test
        // process. We deliberately do NOT await the future — the test just
        // confirms that handler installation doesn't error.
        let result = shutdown_signal();
        assert!(
            result.is_ok(),
            "shutdown_signal() must not fail: {:?}",
            result.err()
        );
        // Drop the future immediately; the handlers are unregistered with it.
    }
}
