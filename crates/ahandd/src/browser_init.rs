use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};
use tracing::info;

const PLAYWRIGHT_CLI_VERSION: &str = "0.1.1";
const NODE_MIN_VERSION: u32 = 20;
const NODE_LTS_VERSION: &str = "24.13.0";

struct Dirs {
    #[allow(dead_code)]
    ahand: PathBuf,
    node: PathBuf,
}

impl Dirs {
    fn new() -> Result<Self> {
        let home = dirs::home_dir().context("cannot determine home directory")?;
        let ahand = home.join(".ahand");
        Ok(Self {
            node: ahand.join("node"),
            ahand,
        })
    }
}

/// Entry point for `ahandd browser-init [--force]`.
pub async fn run(force: bool) -> Result<()> {
    let dirs = Dirs::new()?;

    if force {
        println!("Force mode: cleaning existing installation...");
        clean(&dirs).await;
    }

    let node_bin = ensure_node(&dirs).await?;
    install_playwright_cli(&dirs, &node_bin).await?;

    println!();
    println!("Setup complete!");
    println!("  Node.js:        {}", node_bin.display());
    #[cfg(unix)]
    let cli_path = dirs.node.join("bin").join("playwright-cli");
    #[cfg(windows)]
    let cli_path = dirs.node.join("playwright-cli.cmd");
    println!("  playwright-cli: {}", cli_path.display());
    println!();
    println!("playwright-cli will use the browser installed on your system (Chrome, Edge, etc.).");
    Ok(())
}

async fn clean(dirs: &Dirs) {
    #[cfg(unix)]
    {
        // Uninstall playwright-cli from our managed prefix
        let npm = dirs.node.join("bin").join("npm");
        if npm.exists() {
            let prefix = dirs.node.to_string_lossy().to_string();
            let _ = tokio::process::Command::new(&npm)
                .args(["uninstall", "-g", "--prefix", &prefix, "@playwright/cli"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
    }

    #[cfg(windows)]
    {
        // Kill any lingering Node.js processes before cleaning
        let _ = tokio::process::Command::new("taskkill")
            .args(["/F", "/IM", "node.exe"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        // Uninstall playwright-cli from our managed prefix
        let npm = dirs.node.join("npm.cmd");
        if npm.exists() {
            let prefix = dirs.node.to_string_lossy().to_string();
            let _ = tokio::process::Command::new(&npm)
                .args(["uninstall", "-g", "--prefix", &prefix, "@playwright/cli"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
    }

    println!("  Cleaned browser installation.");
}

// Step 1: Node.js

async fn ensure_node(dirs: &Dirs) -> Result<PathBuf> {
    #[cfg(unix)]
    let local_node = dirs.node.join("bin").join("node");
    #[cfg(windows)]
    let local_node = dirs.node.join("node.exe");

    // Check if we already have a suitable local node
    if local_node.exists() {
        match node_major_version(&local_node).await {
            Some(ver) if ver >= NODE_MIN_VERSION => {
                println!("[1/2] Node.js: v{ver}.x ({})", dirs.node.display());
                return Ok(local_node);
            }
            Some(ver) => {
                println!("  Local node is v{ver}, need >= v{NODE_MIN_VERSION} — will upgrade");
            }
            None => {
                println!(
                    "  Local node at {} exists but failed to determine version — will reinstall",
                    local_node.display()
                );
            }
        }
    }

    // Always install our own node to ~/.ahand/node for a fully self-contained setup.
    // This avoids depending on system node/npm which may have incompatible versions,
    // restrictive .npmrc configs, or require sudo for global installs.
    //
    // Remove the old installation first to avoid stale files from a previous
    // version mixing with the new one (e.g. old npm node_modules).
    if dirs.node.exists() {
        let _ = std::fs::remove_dir_all(&dirs.node);
    }
    println!(
        "  Installing Node.js v{NODE_LTS_VERSION} to {}...",
        dirs.node.display()
    );
    install_node(dirs).await.context(
        "Failed to install Node.js. Check your network connection and retry, \
         or install Node.js >= 20 manually (e.g. `brew install node`).",
    )?;
    if !local_node.exists() {
        anyhow::bail!(
            "Node.js installation completed but binary not found at {}.",
            local_node.display()
        );
    }
    println!(
        "[1/2] Node.js: v{NODE_LTS_VERSION} ({})",
        dirs.node.display()
    );
    Ok(local_node)
}

async fn node_major_version(node_bin: &Path) -> Option<u32> {
    let output = tokio::process::Command::new(node_bin)
        .arg("-v")
        .output()
        .await
        .ok()?;
    let version_str = String::from_utf8_lossy(&output.stdout);
    version_str
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()?
        .parse()
        .ok()
}

async fn install_node(dirs: &Dirs) -> Result<()> {
    #[cfg(windows)]
    {
        let (_os, arch) = platform_info();
        let zipfile = format!("node-v{NODE_LTS_VERSION}-win-{arch}.zip");
        let url = format!("https://nodejs.org/dist/v{NODE_LTS_VERSION}/{zipfile}");

        let bytes = download_bytes(&url).await.context(format!(
            "Failed to download Node.js from {url} — check your network connection"
        ))?;

        std::fs::create_dir_all(&dirs.node).context(format!(
            "Failed to create directory {}: permission denied or disk full",
            dirs.node.display()
        ))?;

        let cursor = std::io::Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor)
            .context("Failed to read Node.js zip archive")?;
        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            let path = file.mangled_name();
            // Strip first component (e.g. "node-v24.13.0-win-x64/node.exe" -> "node.exe")
            let stripped: PathBuf = path.components().skip(1).collect();
            if stripped.components().count() == 0 {
                continue;
            }
            let dest = dirs.node.join(&stripped);
            if file.is_dir() {
                std::fs::create_dir_all(&dest)?;
            } else {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut outfile = std::fs::File::create(&dest)?;
                std::io::copy(&mut file, &mut outfile)?;
            }
        }
    }

    #[cfg(unix)]
    {
        let (os, arch) = platform_info();
        let tarball = format!("node-v{NODE_LTS_VERSION}-{os}-{arch}.tar.xz");
        let url = format!("https://nodejs.org/dist/v{NODE_LTS_VERSION}/{tarball}");

        let bytes = download_bytes(&url).await.context(format!(
            "Failed to download Node.js from {url} — check your network connection"
        ))?;

        std::fs::create_dir_all(&dirs.node).context(format!(
            "Failed to create directory {}: permission denied or disk full",
            dirs.node.display()
        ))?;
        let decoder = xz2::read::XzDecoder::new(std::io::Cursor::new(bytes));
        let mut archive = tar::Archive::new(decoder);
        archive.set_preserve_permissions(true);
        for entry in archive
            .entries()
            .context("Failed to read Node.js archive — download may be corrupted")?
        {
            let mut entry = entry.context("Corrupted entry in Node.js archive")?;
            let path = entry.path()?.into_owned();
            // Strip first component (e.g. "node-v24.13.0-darwin-arm64/bin/node" -> "bin/node")
            let stripped: PathBuf = path.components().skip(1).collect();
            if stripped.components().count() == 0 {
                continue;
            }
            let dest = dirs.node.join(&stripped);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            entry.unpack(&dest).context(format!(
                "Failed to extract {} — disk may be full",
                dest.display()
            ))?;
        }
    }

    Ok(())
}

// Step 2: playwright-cli via npm

async fn install_playwright_cli(dirs: &Dirs, node_bin: &Path) -> Result<()> {
    #[cfg(unix)]
    let npm = node_bin
        .parent()
        .map(|p| p.join("npm"))
        .unwrap_or_else(|| PathBuf::from("npm"));
    #[cfg(windows)]
    let npm = node_bin
        .parent()
        .map(|p| p.join("npm.cmd"))
        .unwrap_or_else(|| PathBuf::from("npm.cmd"));

    // Check if already installed at the correct version
    #[cfg(unix)]
    let cli_path = dirs.node.join("bin").join("playwright-cli");
    #[cfg(windows)]
    let cli_path = dirs.node.join("playwright-cli.cmd");
    if cli_path.exists() {
        // Verify version
        let output = tokio::process::Command::new(&cli_path)
            .arg("--version")
            .output()
            .await;
        if let Ok(out) = output {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if out.status.success() {
                println!(
                    "[2/2] playwright-cli: {ver} ({}) (cached)",
                    cli_path.display()
                );
                return Ok(());
            }
        }
    }

    println!("[2/2] Installing @playwright/cli@{PLAYWRIGHT_CLI_VERSION}...");

    // Always install to ~/.ahand/node via --prefix to avoid permission issues
    // with system npm global prefix (e.g. /usr/local/ requiring sudo).
    let prefix = dirs.node.to_string_lossy().to_string();

    let output = tokio::process::Command::new(&npm)
        .args([
            "install",
            "-g",
            "--prefix",
            &prefix,
            &format!("@playwright/cli@{PLAYWRIGHT_CLI_VERSION}"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("failed to run npm install")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();

        if stderr.contains("EACCES") || stderr.contains("permission denied") {
            anyhow::bail!(
                "Permission denied installing playwright-cli to {prefix}. \
                 Check directory permissions: chmod -R u+w {prefix}"
            );
        }
        if stderr.contains("ETIMEDOUT")
            || stderr.contains("ENOTFOUND")
            || stderr.contains("ECONNREFUSED")
            || stderr.contains("network")
            || stderr.contains("fetch failed")
        {
            anyhow::bail!(
                "Network error installing playwright-cli. \
                 Check your internet connection and proxy settings."
            );
        }
        if stderr.contains("404") || stderr.contains("Not Found") {
            anyhow::bail!(
                "Package @playwright/cli@{PLAYWRIGHT_CLI_VERSION} not found on npm registry. \
                 The version may have been unpublished."
            );
        }
        anyhow::bail!(
            "Failed to install @playwright/cli@{PLAYWRIGHT_CLI_VERSION} (exit {}):\n{}\n\
             Try manually: {} install -g --prefix {} @playwright/cli@{PLAYWRIGHT_CLI_VERSION}",
            output.status.code().unwrap_or(-1),
            stderr,
            npm.display(),
            prefix,
        );
    }

    // Verify installation
    if cli_path.exists() {
        let version = tokio::process::Command::new(&cli_path)
            .arg("--version")
            .output()
            .await
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| "installed".to_string());
        println!("[2/2] playwright-cli: {version} ({})", cli_path.display());
    } else {
        // Fallback: check if npm linked it somewhere else
        if let Ok(fallback) = which("playwright-cli") {
            println!("[2/2] playwright-cli: {} (PATH)", fallback.display());
        } else {
            anyhow::bail!(
                "playwright-cli was installed but binary not found at {}. \
                 Try: {} install -g --prefix {} @playwright/cli@{PLAYWRIGHT_CLI_VERSION}",
                cli_path.display(),
                npm.display(),
                prefix,
            );
        }
    }

    Ok(())
}

// Helpers

async fn download_bytes(url: &str) -> Result<Vec<u8>> {
    info!(url, "downloading");
    let client = reqwest::Client::new();
    let resp = client
        .get(url)
        .header("User-Agent", "ahandd")
        .send()
        .await
        .context("HTTP request failed")?;

    if !resp.status().is_success() {
        anyhow::bail!("HTTP {} for {}", resp.status(), url);
    }

    let bytes = resp.bytes().await.context("failed to read response body")?;
    Ok(bytes.to_vec())
}

fn platform_info() -> (&'static str, &'static str) {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "win"
    } else {
        "unknown"
    };

    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86_64") {
        "x64"
    } else {
        "unknown"
    };

    (os, arch)
}

fn which(bin: &str) -> Result<PathBuf> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        let p = dir.join(bin);
        if p.exists() {
            return Ok(p);
        }
    }
    anyhow::bail!("{bin} not found in PATH")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_info_returns_valid() {
        let (os, arch) = platform_info();
        assert!(
            ["darwin", "linux", "win", "unknown"].contains(&os),
            "unexpected os: {os}"
        );
        assert!(
            ["arm64", "x64", "unknown"].contains(&arch),
            "unexpected arch: {arch}"
        );
    }

    #[test]
    fn dirs_new_succeeds() {
        // Should not panic as long as home dir is available
        let dirs = Dirs::new();
        assert!(dirs.is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn dirs_node_path_is_under_ahand() {
        let dirs = Dirs::new().unwrap();
        assert!(dirs.node.to_string_lossy().contains(".ahand"));
        assert!(dirs.node.to_string_lossy().ends_with("node"));
    }
}
