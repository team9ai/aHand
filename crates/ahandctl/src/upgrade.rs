use anyhow::{Context, Result};

use crate::github_release::{self, GITHUB_REPO};

pub async fn run(check_only: bool, target_version: Option<String>) -> Result<()> {
    let current = current_version();
    let latest = match target_version {
        Some(v) => v,
        None => github_release::fetch_latest_version().await?,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_version_returns_cargo_version() {
        let version = current_version();
        assert!(!version.is_empty());
        // Should be semver-like
        assert!(version.contains('.'), "expected semver: {version}");
    }
}

async fn download_and_install(version: &str) -> Result<()> {
    let (suffix, exe_ext) = github_release::platform_suffix();
    let bin_dir = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".ahand")
        .join("bin");
    std::fs::create_dir_all(&bin_dir)?;

    // Stop daemon before replacing binaries
    if let Err(e) = crate::daemon::stop().await {
        eprintln!("Note: could not stop daemon: {e}");
    }

    // Download checksums for verification
    let checksums_url = format!(
        "https://github.com/{GITHUB_REPO}/releases/download/rust-v{version}/checksums-rust-{suffix}.txt"
    );
    let checksums_bytes = github_release::download_bytes(&checksums_url)
        .await
        .context("Failed to download checksums — cannot verify binary integrity")?;
    let checksums_text = String::from_utf8_lossy(&checksums_bytes).to_string();

    for binary in &["ahandd", "ahandctl"] {
        let asset = format!("{binary}-{suffix}{exe_ext}");
        let url = format!(
            "https://github.com/{GITHUB_REPO}/releases/download/rust-v{version}/{asset}"
        );
        println!("  Downloading {asset}...");
        let bytes = github_release::download_bytes(&url)
            .await
            .with_context(|| format!("Failed to download {asset}"))?;

        // Verify checksum if available
        if let Some(expected) = checksums_text
            .lines()
            .find(|line| line.ends_with(&asset))
            .and_then(|line| line.split_whitespace().next())
        {
            use sha2::{Digest, Sha256};
            let actual = format!("{:x}", Sha256::digest(&bytes));
            if actual != expected {
                anyhow::bail!("Checksum mismatch for {asset}: expected {expected}, got {actual}");
            }
            println!("  Checksum OK: {asset}");
        } else {
            anyhow::bail!("Checksum entry missing for {asset} — cannot verify binary integrity");
        }

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
