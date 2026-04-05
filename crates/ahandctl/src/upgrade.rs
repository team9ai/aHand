use anyhow::{Context, Result};

const GITHUB_REPO: &str = "team9ai/aHand";

pub async fn run(check_only: bool, target_version: Option<String>) -> Result<()> {
    let current = current_version();
    let latest = match target_version {
        Some(v) => v,
        None => fetch_latest_version().await?,
    };

    println!("Current: {current}");
    println!("Latest:  {latest}");

    if current == latest {
        println!("Already up to date.");
        return Ok(());
    }

    if check_only {
        println!("Update available: {current} → {latest}");
        return Ok(());
    }

    println!("Upgrading {current} → {latest}...");
    download_and_install(&latest).await?;
    println!("Upgrade complete. Restart the daemon to use the new version.");
    Ok(())
}

fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

async fn fetch_latest_version() -> Result<String> {
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

async fn download_and_install(version: &str) -> Result<()> {
    let (suffix, exe_ext) = platform_suffix();
    let bin_dir = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".ahand")
        .join("bin");
    std::fs::create_dir_all(&bin_dir)?;

    // Stop daemon before replacing binaries
    if let Err(e) = crate::daemon::stop().await {
        eprintln!("Note: could not stop daemon: {e}");
    }

    for binary in &["ahandd", "ahandctl"] {
        let asset = format!("{binary}-{suffix}{exe_ext}");
        let url = format!(
            "https://github.com/{GITHUB_REPO}/releases/download/rust-v{version}/{asset}"
        );
        println!("  Downloading {asset}...");
        let bytes = download_bytes(&url)
            .await
            .with_context(|| format!("Failed to download {asset}"))?;
        let dest = bin_dir.join(format!("{binary}{exe_ext}"));

        // On Windows, rename current binary before overwriting (can't overwrite running exe)
        #[cfg(windows)]
        {
            let backup = bin_dir.join(format!("{binary}.old{exe_ext}"));
            let _ = std::fs::remove_file(&backup);
            if dest.exists() {
                std::fs::rename(&dest, &backup)
                    .with_context(|| format!("Failed to backup {}", dest.display()))?;
            }
        }

        std::fs::write(&dest, &bytes)
            .with_context(|| format!("Failed to write {}", dest.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
        }

        println!("  Installed: {}", dest.display());
    }
    Ok(())
}

fn platform_suffix() -> (&'static str, &'static str) {
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

async fn download_bytes(url: &str) -> Result<Vec<u8>> {
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
