//! Native upgrade-check and (future) upgrade execution for `ahandctl upgrade`.

mod release;

pub use release::{ReleaseInfo, current_version, resolve_latest, resolve_target};

use std::path::{Path, PathBuf};

use anyhow::Context as _;

/// GitHub API base URL — injected by tests via [`run_with_bases`].
const DEFAULT_API_BASE: &str = "https://api.github.com";

/// Default GitHub repository slug.
const DEFAULT_REPO: &str = "team9ai/aHand";

// ── Public entry point ─────────────────────────────────────────────────────

/// Run the upgrade subcommand.
///
/// Reads `AHAND_DIR` for the aHand home directory, `AHAND_VERSION` for a
/// version pin (overridden by `target_version`), and delegates to
/// [`run_with_bases`] for testability.
pub async fn run(check_only: bool, target_version: Option<String>) -> anyhow::Result<()> {
    let ahand_home = resolve_ahand_home()?;

    // CLI arg takes precedence over the AHAND_VERSION env var.
    let version_override = target_version.or_else(|| std::env::var("AHAND_VERSION").ok());

    run_with_bases(
        check_only,
        version_override.as_deref(),
        DEFAULT_API_BASE,
        &ahand_home,
    )
    .await
}

/// Testable seam: run the upgrade command with injected API base and aHand home.
///
/// This keeps [`run`] a thin wrapper so integration tests can inject a local
/// stub server (no network) and a temporary directory (no marker file
/// pollution) without touching environment variables.
pub async fn run_with_bases(
    check_only: bool,
    version_override: Option<&str>,
    api_base: &str,
    ahand_home: &Path,
) -> anyhow::Result<()> {
    if check_only {
        let output = check_output(version_override, api_base, ahand_home).await?;
        print!("{}", output);
        return Ok(());
    }

    let cur = current_version(ahand_home);
    let info = resolve_target(version_override, api_base, DEFAULT_REPO).await?;
    perform_upgrade(&cur, &info, api_base, ahand_home).await
}

/// Build and return the check-mode output string.
///
/// Factored out of [`run_with_bases`] so tests can call it directly and
/// inspect the returned string without stdout capture.
pub async fn check_output(
    version_override: Option<&str>,
    api_base: &str,
    ahand_home: &Path,
) -> anyhow::Result<String> {
    let cur = current_version(ahand_home);
    let info = resolve_target(version_override, api_base, DEFAULT_REPO).await?;

    let latest_rust = info
        .rust
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Could not determine latest version"))?;

    let suffix = ahand_platform::paths::release_suffix();

    let admin_display = info.admin.as_deref().unwrap_or("none");
    let browser_display = info.browser.as_deref().unwrap_or("none");

    Ok(build_check_output(
        &cur,
        latest_rust,
        admin_display,
        browser_display,
        &suffix,
    ))
}

/// Build the check-mode output string (factored out for unit testing without
/// stdout capture).
pub fn build_check_output(
    current: &str,
    latest_rust: &str,
    admin_display: &str,
    browser_display: &str,
    suffix: &str,
) -> String {
    let status_line = if current == latest_rust {
        "Already up to date!\n".to_string()
    } else {
        format!(
            "Update available: {} -> {}\nRun: ahandctl upgrade\n",
            current, latest_rust
        )
    };

    format!(
        "Current version: {current}\nLatest version:  rust={latest_rust} admin={admin_display} browser={browser_display}\nPlatform:        {suffix}\n{status_line}"
    )
}

/// Perform the actual upgrade.
///
/// NOTE: Full native upgrade implementation lands in the next change (Task 3).
/// For now this stub returns an error with the pinned message below so callers
/// can distinguish "not yet implemented" from a real failure.
async fn perform_upgrade(
    _current: &str,
    _info: &ReleaseInfo,
    _api_base: &str,
    _ahand_home: &Path,
) -> anyhow::Result<()> {
    anyhow::bail!("full native upgrade lands in the next change; use --check to query versions")
}

// ── Private helpers ────────────────────────────────────────────────────────

fn resolve_ahand_home() -> anyhow::Result<PathBuf> {
    if let Ok(dir) = std::env::var("AHAND_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".ahand"))
}
