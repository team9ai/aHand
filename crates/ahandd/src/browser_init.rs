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
    let cli_path = dirs.node.join("bin").join("playwright-cli");
    println!("  playwright-cli: {}", cli_path.display());
    println!();
    println!("playwright-cli will use the browser installed on your system (Chrome, Edge, etc.).");
    Ok(())
}

async fn clean(dirs: &Dirs) {
    // Uninstall playwright-cli from npm globals
    let npm = dirs.node.join("bin").join("npm");
    if npm.exists() {
        let _ = tokio::process::Command::new(&npm)
            .args(["uninstall", "-g", "@playwright/cli"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
    println!("  Cleaned playwright-cli installation.");
}

// Step 1: Node.js

async fn ensure_node(dirs: &Dirs) -> Result<PathBuf> {
    // 1. Check locally installed node (~/.ahand/node/bin/node)
    let local_node = dirs.node.join("bin").join("node");
    if local_node.exists() {
        match node_major_version(&local_node).await {
            Some(ver) if ver >= NODE_MIN_VERSION => {
                println!("[1/2] Node.js: v{ver}.x (local: {})", dirs.node.display());
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

    // 2. Check system node (PATH)
    if let Ok(system_node) = which("node") {
        match node_major_version(&system_node).await {
            Some(ver) if ver >= NODE_MIN_VERSION => {
                println!("[1/2] Node.js: v{ver}.x (system: {})", system_node.display());
                return Ok(system_node);
            }
            Some(ver) => {
                println!(
                    "  System node is v{ver} (at {}), need >= v{NODE_MIN_VERSION}",
                    system_node.display()
                );
            }
            None => {
                println!(
                    "  Found node at {} but failed to determine version",
                    system_node.display()
                );
            }
        }
    }

    // 3. Auto-install
    println!("  No suitable Node.js found, installing v{NODE_LTS_VERSION}...");
    install_node(dirs).await.context(
        "Failed to auto-install Node.js. You can install Node.js >= 20 manually \
         (e.g. `brew install node` or https://nodejs.org) and retry.",
    )?;
    let node_bin = dirs.node.join("bin").join("node");
    if !node_bin.exists() {
        anyhow::bail!(
            "Node.js installation completed but binary not found at {}. \
             Please install Node.js >= {NODE_MIN_VERSION} manually and retry.",
            node_bin.display()
        );
    }
    println!("[1/2] Node.js: v{NODE_LTS_VERSION} (installed to {})", dirs.node.display());
    Ok(node_bin)
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
    for entry in archive.entries().context("Failed to read Node.js archive — download may be corrupted")? {
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

    Ok(())
}

// Step 2: playwright-cli via npm

async fn install_playwright_cli(dirs: &Dirs, node_bin: &Path) -> Result<()> {
    let npm = node_bin
        .parent()
        .map(|p| p.join("npm"))
        .unwrap_or_else(|| PathBuf::from("npm"));

    // Check if already installed at the correct version
    let cli_path = dirs.node.join("bin").join("playwright-cli");
    if cli_path.exists() {
        // Verify version
        let output = tokio::process::Command::new(&cli_path)
            .arg("--version")
            .output()
            .await;
        if let Ok(out) = output {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if out.status.success() {
                println!("[2/2] playwright-cli: {ver} ({}) (cached)", cli_path.display());
                return Ok(());
            }
        }
    }

    println!("  Installing @playwright/cli@{PLAYWRIGHT_CLI_VERSION}...");

    let status = tokio::process::Command::new(&npm)
        .args(["install", "-g", &format!("@playwright/cli@{PLAYWRIGHT_CLI_VERSION}")])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context("failed to run npm install")?;

    if !status.success() {
        anyhow::bail!(
            "Failed to install @playwright/cli@{PLAYWRIGHT_CLI_VERSION}. \
             Try manually: {} install -g @playwright/cli@{PLAYWRIGHT_CLI_VERSION}",
            npm.display()
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
                 Try: {} install -g @playwright/cli@{PLAYWRIGHT_CLI_VERSION}",
                cli_path.display(),
                npm.display()
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
