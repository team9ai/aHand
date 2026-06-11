//! GitHub Releases API helpers for resolving the latest aHand version tags.

use std::path::Path;

use anyhow::Context as _;

/// Resolved version tags for the three aHand release artefacts.
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    /// Latest `rust-v*` tag (the core daemon + CLI).
    pub rust: Option<String>,
    /// Latest `admin-v*` tag; `None` if no matching tag exists.
    pub admin: Option<String>,
    /// Latest `browser-v*` tag; `None` if no matching tag exists.
    pub browser: Option<String>,
}

/// Resolve the latest release tags from the GitHub Releases API.
///
/// Sends `GET {api_base}/repos/{repo}/releases` and returns the first tag
/// name that starts with each of `rust-v`, `admin-v`, and `browser-v`.
///
/// # Notes
/// - The GitHub Releases API returns entries newest-first, so the *first*
///   matching tag is the latest for each prefix.
/// - Unauthenticated GitHub API calls are subject to a rate-limit of 60
///   requests per hour per IP — the same exposure as the legacy
///   `upgrade.sh`; acceptable for a manual CLI command.
pub async fn resolve_latest(api_base: &str, repo: &str) -> anyhow::Result<ReleaseInfo> {
    let url = format!("{api_base}/repos/{repo}/releases");
    let client = reqwest::Client::builder()
        .user_agent("ahandctl-upgrade")
        .build()
        .context("failed to build HTTP client")?;

    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url} failed"))?;

    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("GitHub API returned HTTP {} for {}", status, url);
    }

    let body = resp
        .text()
        .await
        .with_context(|| format!("failed to read response body from {url}"))?;

    parse_releases(&body).with_context(|| format!("failed to parse releases JSON from {url}"))
}

/// Resolve the target release info, applying an optional version override.
///
/// If `version_override` is `Some(v)`, all three artefact versions are pinned
/// to `v` (wrapping `Some(v.to_string())`); the GitHub API is **not** called.
/// This mirrors the `--version X` / `AHAND_VERSION` behaviour of the legacy
/// `upgrade.sh`.
pub async fn resolve_target(
    version_override: Option<&str>,
    api_base: &str,
    repo: &str,
) -> anyhow::Result<ReleaseInfo> {
    if let Some(v) = version_override {
        let owned = v.to_string();
        return Ok(ReleaseInfo {
            rust: Some(owned.clone()),
            admin: Some(owned.clone()),
            browser: Some(owned),
        });
    }
    resolve_latest(api_base, repo).await
}

/// Return the currently-installed aHand version.
///
/// Reads the version marker written by the installer at
/// `{ahand_home}/version`.  If the marker is absent (e.g. a dev build run
/// directly from `cargo run`), falls back to the `CARGO_PKG_VERSION` compiled
/// into this binary — the same value that `ahandctl --version` would print,
/// which is what the legacy `upgrade.sh` fell back to when the marker was
/// missing.
pub fn current_version(ahand_home: &Path) -> String {
    ahand_platform::paths::read_version_marker(ahand_home)
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

// ── Internal helpers ───────────────────────────────────────────────────────

/// Parse a GitHub Releases JSON array and extract the first tag per prefix.
fn parse_releases(body: &str) -> anyhow::Result<ReleaseInfo> {
    let releases: serde_json::Value =
        serde_json::from_str(body).context("releases JSON is not valid")?;

    let arr = releases
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("releases JSON is not an array"))?;

    let mut rust: Option<String> = None;
    let mut admin: Option<String> = None;
    let mut browser: Option<String> = None;

    for entry in arr {
        let tag = match entry.get("tag_name").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => continue,
        };
        if rust.is_none() && tag.starts_with("rust-v") {
            rust = Some(tag.to_string());
        }
        if admin.is_none() && tag.starts_with("admin-v") {
            admin = Some(tag.to_string());
        }
        if browser.is_none() && tag.starts_with("browser-v") {
            browser = Some(tag.to_string());
        }
        // Short-circuit once all three are found.
        if rust.is_some() && admin.is_some() && browser.is_some() {
            break;
        }
    }

    Ok(ReleaseInfo {
        rust,
        admin,
        browser,
    })
}
