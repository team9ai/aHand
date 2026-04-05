use anyhow::{Context, Result};

pub const GITHUB_REPO: &str = "team9ai/aHand";

/// Fetch the latest release version from GitHub (strips the `rust-v` prefix).
pub async fn fetch_latest_version() -> Result<String> {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("User-Agent", "ahandctl")
        .send()
        .await
        .context("Failed to fetch latest release")?
        .json::<serde_json::Value>()
        .await
        .context("Failed to parse release response")?;
    let tag = resp["tag_name"]
        .as_str()
        .context("no tag_name in release")?;
    Ok(tag.strip_prefix("rust-v").unwrap_or(tag).to_string())
}

/// Returns `(platform_suffix, exe_extension)` for the current target.
pub fn platform_suffix() -> (&'static str, &'static str) {
    if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        ("darwin-arm64", "")
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "x86_64") {
        ("darwin-x64", "")
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        ("linux-x64", "")
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "aarch64") {
        ("linux-arm64", "")
    } else if cfg!(target_os = "windows") && cfg!(target_arch = "x86_64") {
        ("windows-x64", ".exe")
    } else {
        ("unknown", "")
    }
}

/// Download raw bytes from `url` with a User-Agent header.
pub async fn download_bytes(url: &str) -> Result<Vec<u8>> {
    let client = reqwest::Client::new();
    let resp = client
        .get(url)
        .header("User-Agent", "ahandctl")
        .send()
        .await
        .context("HTTP request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {} for {}", resp.status(), url);
    }
    let bytes = resp.bytes().await.context("Failed to read response body")?;
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_suffix_returns_valid_tuple() {
        let (suffix, ext) = platform_suffix();
        // On macOS CI: ("darwin-arm64", "") or ("darwin-x64", "")
        // On Linux CI: ("linux-x64", "") or ("linux-arm64", "")
        // On Windows CI: ("windows-x64", ".exe")
        assert!(!suffix.is_empty());
        assert!(
            suffix.starts_with("darwin-")
                || suffix.starts_with("linux-")
                || suffix.starts_with("windows-")
                || suffix == "unknown",
            "unexpected suffix: {suffix}"
        );

        if suffix.starts_with("windows-") {
            assert_eq!(ext, ".exe");
        } else if suffix != "unknown" {
            assert_eq!(ext, "");
        }
    }

    #[test]
    fn platform_suffix_is_deterministic() {
        let a = platform_suffix();
        let b = platform_suffix();
        assert_eq!(a, b);
    }
}
