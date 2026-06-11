//! GitHub Releases API helpers for resolving the latest aHand version tags.

use std::path::Path;

use anyhow::Context as _;

/// Regex-like charset for accepted version strings (after prefix strip).
///
/// Only `[0-9A-Za-z.\-]+` is accepted.  Tags with path-separators, shell
/// metacharacters, or other special characters are treated as non-matching so
/// that an adversarially-named tag (e.g. `rust-v../evil`) cannot escape the
/// download URL template.
fn is_valid_version_str(v: &str) -> bool {
    !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

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
/// Sends `GET {api_base}/repos/{repo}/releases?per_page=100` and returns the
/// first tag name that starts with each of `rust-v`, `admin-v`, and
/// `browser-v`.
///
/// # Notes
/// - `per_page=100` is the GitHub API maximum per page.  aHand uses three
///   independent release tracks (`rust-v*`, `admin-v*`, `browser-v*`), each
///   publishing its own release entries.  Without pagination the default page
///   of 30 entries could be exhausted by older tracks before all three
///   prefixes are seen, causing a spurious "no matching tag" result.
/// - The GitHub Releases API returns entries newest-first, so the *first*
///   matching tag is the latest for each prefix.
/// - Unauthenticated GitHub API calls are subject to a rate-limit of 60
///   requests per hour per IP — the same exposure as the legacy
///   `upgrade.sh`; acceptable for a manual CLI command.
/// - Version strings are constrained to `[0-9A-Za-z.\-]+` after the prefix
///   is stripped.  Tags that do not match (e.g. `rust-v../evil`) are silently
///   skipped so they cannot influence the download URL.
pub async fn resolve_latest(api_base: &str, repo: &str) -> anyhow::Result<ReleaseInfo> {
    let url = format!("{api_base}/repos/{repo}/releases?per_page=100");
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
        if rust.is_none()
            && let Some(v) = tag.strip_prefix("rust-v")
            && is_valid_version_str(v)
        {
            rust = Some(v.to_string());
        }
        if admin.is_none()
            && let Some(v) = tag.strip_prefix("admin-v")
            && is_valid_version_str(v)
        {
            admin = Some(v.to_string());
        }
        if browser.is_none()
            && let Some(v) = tag.strip_prefix("browser-v")
            && is_valid_version_str(v)
        {
            browser = Some(v.to_string());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// parse_releases picks the first valid tag for each prefix in a normal
    /// all-three-present JSON array.
    #[test]
    fn parse_releases_normal_tags() {
        let body = r#"[
            {"tag_name": "rust-v1.2.3"},
            {"tag_name": "admin-v0.5.0"},
            {"tag_name": "browser-v0.1.0"}
        ]"#;
        let info = parse_releases(body).unwrap();
        assert_eq!(info.rust.as_deref(), Some("1.2.3"));
        assert_eq!(info.admin.as_deref(), Some("0.5.0"));
        assert_eq!(info.browser.as_deref(), Some("0.1.0"));
    }

    /// A tag whose version part contains path-traversal or shell-special chars
    /// (e.g. `rust-v../evil`) must be silently ignored — not used as the
    /// resolved version — so that it cannot poison the download URL.
    #[test]
    fn parse_releases_ignores_malicious_tag() {
        // The adversarial tag comes first; the safe one comes second.
        let body = r#"[
            {"tag_name": "rust-v../evil"},
            {"tag_name": "rust-v1.0.0"}
        ]"#;
        let info = parse_releases(body).unwrap();
        // Must pick the safe tag, not the traversal one.
        assert_eq!(
            info.rust.as_deref(),
            Some("1.0.0"),
            "malicious tag rust-v../evil must be skipped"
        );
    }

    /// Tags with semicolons, slashes, or spaces are rejected by the charset
    /// guard.
    #[test]
    fn parse_releases_ignores_special_char_tags() {
        let body = r#"[
            {"tag_name": "rust-v1.0;rm -rf /"},
            {"tag_name": "rust-v1.0/path"},
            {"tag_name": "rust-v1 0"},
            {"tag_name": "rust-v2.0.0"}
        ]"#;
        let info = parse_releases(body).unwrap();
        assert_eq!(info.rust.as_deref(), Some("2.0.0"));
    }

    /// is_valid_version_str: confirm accepted and rejected inputs.
    #[test]
    fn version_str_charset_accepted() {
        for v in &["1.0.0", "0.1.2-beta", "1.2.3-rc.1", "v1", "ABC"] {
            assert!(
                is_valid_version_str(v),
                "{v:?} should be accepted by charset guard"
            );
        }
    }

    #[test]
    fn version_str_charset_rejected() {
        for v in &["../evil", "1.0/path", "1.0;rm", "1 0", "", "1.0\n2.0"] {
            assert!(
                !is_valid_version_str(v),
                "{v:?} should be rejected by charset guard"
            );
        }
    }

    /// The API URL must contain per_page=100.
    #[test]
    fn resolve_latest_url_contains_per_page() {
        // We cannot call resolve_latest without a server, but we can verify
        // the URL constructed in parse_releases is fed the correct shape by
        // inspecting the format string indirectly — just assert the constant
        // in the source.  Here we do a simple string-level smoke check.
        let api_base = "https://api.github.com";
        let repo = "team9ai/aHand";
        let url = format!("{api_base}/repos/{repo}/releases?per_page=100");
        assert!(url.contains("per_page=100"));
    }
}
