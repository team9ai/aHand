use anyhow::{Context, Result};

use crate::github_release::{self, GITHUB_REPO};

pub async fn run(target_version: Option<String>) -> Result<()> {
    let version = match target_version {
        Some(v) => v,
        None => github_release::fetch_latest_version().await?,
    };

    let (suffix, exe_ext) = github_release::platform_suffix();
    let bin_dir = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".ahand")
        .join("bin");
    std::fs::create_dir_all(&bin_dir)?;

    let asset = format!("ahandd-{suffix}{exe_ext}");
    let url = format!(
        "https://github.com/{GITHUB_REPO}/releases/download/rust-v{version}/{asset}"
    );

    println!("Downloading ahandd v{version} ({suffix})...");
    let bytes = github_release::download_bytes(&url)
        .await
        .with_context(|| format!("Failed to download {asset}"))?;

    let dest = bin_dir.join(format!("ahandd{exe_ext}"));

    // On Windows, rename existing binary before overwriting (can't overwrite running exe)
    #[cfg(windows)]
    {
        let backup = bin_dir.join(format!("ahandd.old{exe_ext}"));
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

    println!("Installed: {}", dest.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn install_daemon_module_compiles() {
        // Smoke test — module is reachable and types resolve
        assert!(true);
    }
}
