//! Unified shutdown signal: SIGTERM/SIGINT on Unix; Ctrl-C and console-close
//! on Windows (Ctrl-Break/logoff/shutdown events are NOT handled). Handlers
//! are registered eagerly when `shutdown_signal()` is called; installation
//! failure is returned as an error (fail-closed on both platforms). The
//! returned future yields the *name* of the signal that fired, for logs.

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
    use anyhow::Context as _;
    use tokio::signal::windows::{ctrl_c, ctrl_close};
    // Register eagerly so install failures surface at startup (parity with
    // the Unix path) instead of resolving into a spurious instant shutdown.
    let mut c = ctrl_c().context("install Ctrl-C handler")?;
    let mut close = ctrl_close().context("install console-close handler")?;
    Ok(async move {
        tokio::select! {
            _ = c.recv() => "ctrl-c",
            _ = close.recv() => "console-close",
        }
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
        // Drop the future without polling; this only checks handler installation
        // succeeds — OS-level dispositions remain installed process-wide (tokio
        // semantics).
    }
}
