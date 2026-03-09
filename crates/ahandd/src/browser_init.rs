use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};
use tracing::info;

const AGENT_BROWSER_VERSION: &str = "0.9.1";
const AGENT_BROWSER_REPO: &str = "vercel-labs/agent-browser";
const AHAND_GITHUB_REPO: &str = "team9ai/aHand";
const NODE_MIN_VERSION: u32 = 20;
const NODE_LTS_VERSION: &str = "24.13.0";

struct Dirs {
    #[allow(dead_code)]
    ahand: PathBuf,
    bin: PathBuf,
    browser: PathBuf,
    node: PathBuf,
}

impl Dirs {
    fn new() -> Result<Self> {
        let home = dirs::home_dir().context("cannot determine home directory")?;
        let ahand = home.join(".ahand");
        Ok(Self {
            bin: ahand.join("bin"),
            browser: ahand.join("browser"),
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
    download_agent_browser(&dirs).await?;
    download_daemon_bundle(&dirs).await?;

    let sockets_dir = dirs.browser.join("sockets");
    std::fs::create_dir_all(&sockets_dir)?;
    println!("[4/6] Socket directory: {}", sockets_dir.display());

    detect_or_install_browser(&dirs, &node_bin).await?;
    write_runtime_config(&dirs, &node_bin)?;

    println!();
    println!("Browser setup complete!");
    Ok(())
}

async fn clean(dirs: &Dirs) {
    #[cfg(unix)]
    {
        let _ = tokio::process::Command::new("pkill")
            .args(["-f", "daemon.js"])
            .status()
            .await;
    }
    #[cfg(windows)]
    {
        let _ = tokio::process::Command::new("taskkill")
            .args(["/F", "/IM", "node.exe", "/FI", "WINDOWTITLE eq daemon.js"])
            .status()
            .await;
    }

    let sockets = dirs.browser.join("sockets");
    if sockets.exists() {
        let _ = std::fs::remove_dir_all(&sockets);
    }
    println!("  Cleaned sockets and stale processes.");
}

// Step 1: Node.js

async fn ensure_node(dirs: &Dirs) -> Result<PathBuf> {
    let local_node = dirs.node.join("bin").join("node");
    if local_node.exists() {
        if let Some(ver) = node_major_version(&local_node).await {
            if ver >= NODE_MIN_VERSION {
                println!("[1/6] Node.js: v{ver}.x (local: {})", dirs.node.display());
                return Ok(local_node);
            }
        }
    }

    if let Ok(system_node) = which("node") {
        if let Some(ver) = node_major_version(&system_node).await {
            if ver >= NODE_MIN_VERSION {
                println!("[1/6] Node.js: v{ver}.x (system)");
                return Ok(system_node);
            }
            println!("  System node is v{ver}, need >= v{NODE_MIN_VERSION}");
        }
    }

    println!("  Installing Node.js v{NODE_LTS_VERSION}...");
    install_node(dirs).await?;
    let node_bin = dirs.node.join("bin").join("node");
    println!("[1/6] Node.js: v{NODE_LTS_VERSION} (installed to {})", dirs.node.display());
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

    let bytes = download_bytes(&url).await?;

    std::fs::create_dir_all(&dirs.node)?;
    let decoder = xz2::read::XzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    archive.set_preserve_permissions(true);
    for entry in archive.entries()? {
        let mut entry = entry?;
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
        entry.unpack(&dest)?;
    }

    Ok(())
}

// Step 2: agent-browser CLI

async fn download_agent_browser(dirs: &Dirs) -> Result<()> {
    std::fs::create_dir_all(&dirs.bin)?;
    let dest = dirs.bin.join("agent-browser");

    // Skip download if the binary already exists (unless --force was used,
    // which cleans the installation before reaching this point).
    if dest.exists() {
        println!("[2/6] CLI binary: {} (cached)", dest.display());
        return Ok(());
    }

    let (os, arch) = platform_info();
    let binary_name = format!("agent-browser-{os}-{arch}");
    let url = format!(
        "https://github.com/{AGENT_BROWSER_REPO}/releases/download/v{AGENT_BROWSER_VERSION}/{binary_name}"
    );

    println!("  Downloading agent-browser v{AGENT_BROWSER_VERSION} ({os}-{arch})...");
    let bytes = download_bytes(&url).await?;
    std::fs::write(&dest, &bytes)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
    }

    println!("[2/6] CLI binary: {}", dest.display());
    Ok(())
}

// Step 3: daemon.js bundle

async fn download_daemon_bundle(dirs: &Dirs) -> Result<()> {
    let dist_dir = dirs.browser.join("dist");
    let daemon_js = dist_dir.join("daemon.js");

    // Skip download if daemon.js already exists.
    if daemon_js.exists() {
        println!("[3/6] Daemon bundle: {} (cached)", dist_dir.display());
        return Ok(());
    }

    let version = fetch_latest_browser_release_version().await?;
    println!("  Downloading daemon bundle v{version}...");

    let url = format!(
        "https://github.com/{AHAND_GITHUB_REPO}/releases/download/browser-v{version}/daemon-bundle.tar.gz"
    );
    let bytes = download_bytes(&url).await?;

    std::fs::create_dir_all(&dist_dir)?;

    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(&dist_dir)?;

    std::fs::write(dist_dir.join("package.json"), "{\"type\":\"module\"}")?;

    println!("[3/6] Daemon bundle: {}", dist_dir.display());
    Ok(())
}

async fn fetch_latest_browser_release_version() -> Result<String> {
    let url = format!("https://api.github.com/repos/{AHAND_GITHUB_REPO}/releases");
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("User-Agent", "ahandd")
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    let releases = resp.as_array().context("GitHub releases response is not an array")?;
    for release in releases {
        if let Some(tag) = release["tag_name"].as_str() {
            if let Some(ver) = tag.strip_prefix("browser-v") {
                return Ok(ver.to_string());
            }
        }
    }

    anyhow::bail!("no browser-v* release found on GitHub")
}

// Step 5: Browser detection

async fn detect_or_install_browser(dirs: &Dirs, node_bin: &Path) -> Result<()> {
    if let Some(chrome) = detect_system_chrome() {
        println!("[5/6] Browser: {chrome} (system)");
        return Ok(());
    }

    println!("[5/6] Browser: no system Chrome found — installing Chromium...");
    let browsers_dir = dirs.browser.join("browsers");
    std::fs::create_dir_all(&browsers_dir)?;

    let npx = node_bin
        .parent()
        .map(|p| p.join("npx"))
        .unwrap_or_else(|| PathBuf::from("npx"));

    let status = tokio::process::Command::new(&npx)
        .args(["playwright", "install", "chromium"])
        .env("PLAYWRIGHT_BROWSERS_PATH", &browsers_dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context("failed to run npx playwright install")?;

    if !status.success() {
        anyhow::bail!("Chromium installation failed");
    }

    println!("      Chromium installed to {}", browsers_dir.display());
    Ok(())
}

fn detect_system_chrome() -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        for candidate in &[
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Google Chrome Dev.app/Contents/MacOS/Google Chrome Dev",
            "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ] {
            if Path::new(candidate).exists() {
                return Some(candidate);
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        for candidate in &[
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
        ] {
            if Path::new(candidate).exists() {
                return Some(candidate);
            }
        }
    }
    None
}

// Step 6: Runtime config

fn write_runtime_config(dirs: &Dirs, node_bin: &Path) -> Result<()> {
    let chrome_path = detect_system_chrome().unwrap_or("");
    let agent_browser_bin = dirs.bin.join("agent-browser");
    let content = format!(
        r#"# Auto-generated by ahandd browser-init — do not edit.
NODE_BIN="{}"
AGENT_BROWSER_BIN="{}"
AGENT_BROWSER_VERSION="{AGENT_BROWSER_VERSION}"
CHROME_PATH="{chrome_path}"
BROWSER_DIR="{}"
"#,
        node_bin.display(),
        agent_browser_bin.display(),
        dirs.browser.display(),
    );
    std::fs::write(dirs.browser.join("env.sh"), content)?;
    println!("[6/6] Runtime config: {}", dirs.browser.join("env.sh").display());
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
