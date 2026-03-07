# ahandd browser-init Subcommand Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Move browser-init logic from shell script into ahandd as a native Rust subcommand, so team9 (and any integrator) can call `ahandd browser-init` without needing ahandctl.

**Architecture:** Add clap subcommand support to ahandd's main.rs. No subcommand = daemon mode (existing behavior). `browser-init` subcommand runs the setup steps in Rust and exits. Reuse existing path conventions from `browser.rs` and `config.rs`.

**Tech Stack:** Rust, reqwest (HTTP downloads), tar + flate2 (tar.gz extraction), xz2 (tar.xz extraction for Node.js), clap (CLI)

---

## Constants (from setup-browser.sh)

```
AGENT_BROWSER_REPO = "vercel-labs/agent-browser"
AGENT_BROWSER_VERSION = "0.9.1"
NODE_MIN_VERSION = 20
NODE_LTS_VERSION = "24.13.0"
GITHUB_REPO = "team9ai/aHand"
AHAND_DIR = ~/.ahand
```

---

### Task 1: Add new dependencies to ahandd Cargo.toml

**Files:**
- Modify: `crates/ahandd/Cargo.toml`

**Step 1: Add reqwest, tar, flate2, xz2 dependencies**

Add these lines to `[dependencies]`:

```toml
reqwest = { version = "0.12", features = ["stream"] }
tar = "0.4"
flate2 = "1"
xz2 = "0.1"
tokio-util = { version = "0.7", features = ["io"] }
```

**Step 2: Verify it compiles**

Run: `cargo check -p ahandd`
Expected: compiles with no errors

**Step 3: Commit**

```bash
git add crates/ahandd/Cargo.toml
git commit -m "chore(ahandd): add reqwest/tar/flate2/xz2 deps for browser-init"
```

---

### Task 2: Create browser_init module

**Files:**
- Create: `crates/ahandd/src/browser_init.rs`
- Modify: `crates/ahandd/src/main.rs` (add `mod browser_init;`)

**Step 1: Create the browser_init.rs file**

```rust
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};
use tracing::info;

const AGENT_BROWSER_VERSION: &str = "0.9.1";
const AGENT_BROWSER_REPO: &str = "vercel-labs/agent-browser";
const AHAND_GITHUB_REPO: &str = "team9ai/aHand";
const NODE_MIN_VERSION: u32 = 20;
const NODE_LTS_VERSION: &str = "24.13.0";

/// Resolved paths for the aHand browser setup.
struct Dirs {
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

    // Step 1: Node.js
    let node_bin = ensure_node(&dirs).await?;

    // Step 2: agent-browser CLI binary
    download_agent_browser(&dirs).await?;

    // Step 3: daemon.js bundle
    download_daemon_bundle(&dirs).await?;

    // Step 4: Socket directory
    let sockets_dir = dirs.browser.join("sockets");
    std::fs::create_dir_all(&sockets_dir)?;
    println!("[4/6] Socket directory: {}", sockets_dir.display());

    // Step 5: Browser detection / Chromium install
    detect_or_install_browser(&dirs, &node_bin).await?;

    // Step 6: Write runtime config
    write_runtime_config(&dirs, &node_bin)?;

    println!();
    println!("Browser setup complete!");
    Ok(())
}

/// Clean runtime files (sockets, stale daemon processes).
async fn clean(dirs: &Dirs) {
    // Kill stale daemon processes.
    let _ = tokio::process::Command::new("pkill")
        .args(["-f", "daemon.js"])
        .status()
        .await;

    // Remove sockets.
    let sockets = dirs.browser.join("sockets");
    if sockets.exists() {
        let _ = std::fs::remove_dir_all(&sockets);
    }
    println!("  Cleaned sockets and stale processes.");
}

// ── Step 1: Node.js ─────────────────────────────────────────────────

async fn ensure_node(dirs: &Dirs) -> Result<PathBuf> {
    // 1a. Check locally installed node.
    let local_node = dirs.node.join("bin").join("node");
    if local_node.exists() {
        if let Some(ver) = node_major_version(&local_node).await {
            if ver >= NODE_MIN_VERSION {
                println!("[1/6] Node.js: v{ver}.x (local: {})", dirs.node.display());
                return Ok(local_node);
            }
        }
    }

    // 1b. Check system node.
    if let Ok(system_node) = which("node") {
        if let Some(ver) = node_major_version(&system_node).await {
            if ver >= NODE_MIN_VERSION {
                println!("[1/6] Node.js: v{ver}.x (system)");
                return Ok(system_node);
            }
            println!("  System node is v{ver}, need >= v{NODE_MIN_VERSION}");
        }
    }

    // 1c. Install prebuilt Node.js LTS.
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
    // "v20.11.0\n" → 20
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

    // Extract tar.xz with --strip-components=1
    std::fs::create_dir_all(&dirs.node)?;
    let decoder = xz2::read::XzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    archive.set_preserve_permissions(true);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        // Strip first component (e.g. "node-v24.13.0-darwin-arm64/bin/node" → "bin/node")
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

// ── Step 2: agent-browser CLI ───────────────────────────────────────

async fn download_agent_browser(dirs: &Dirs) -> Result<()> {
    let (os, arch) = platform_info();
    let binary_name = format!("agent-browser-{os}-{arch}");
    let url = format!(
        "https://github.com/{AGENT_BROWSER_REPO}/releases/download/v{AGENT_BROWSER_VERSION}/{binary_name}"
    );

    std::fs::create_dir_all(&dirs.bin)?;
    let dest = dirs.bin.join("agent-browser");

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

// ── Step 3: daemon.js bundle ────────────────────────────────────────

async fn download_daemon_bundle(dirs: &Dirs) -> Result<()> {
    // Fetch latest browser release version from GitHub API.
    let version = fetch_latest_browser_release_version().await?;
    println!("  Downloading daemon bundle v{version}...");

    let url = format!(
        "https://github.com/{AHAND_GITHUB_REPO}/releases/download/browser-v{version}/daemon-bundle.tar.gz"
    );
    let bytes = download_bytes(&url).await?;

    let dist_dir = dirs.browser.join("dist");
    std::fs::create_dir_all(&dist_dir)?;

    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(&dist_dir)?;

    // Write package.json for ESM support.
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

// ── Step 5: Browser detection ───────────────────────────────────────

async fn detect_or_install_browser(dirs: &Dirs, node_bin: &Path) -> Result<()> {
    if let Some(chrome) = detect_system_chrome() {
        println!("[5/6] Browser: {chrome} (system)");
        return Ok(());
    }

    println!("[5/6] Browser: no system Chrome found — installing Chromium...");
    let browsers_dir = dirs.browser.join("browsers");
    std::fs::create_dir_all(&browsers_dir)?;

    // Resolve npx path relative to node binary.
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

// ── Step 6: Runtime config ──────────────────────────────────────────

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

// ── Helpers ─────────────────────────────────────────────────────────

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
```

**Step 2: Add `mod browser_init;` to main.rs**

Add `mod browser_init;` to the module declarations at the top of `crates/ahandd/src/main.rs` (after `mod browser;`).

**Step 3: Verify it compiles**

Run: `cargo check -p ahandd`
Expected: compiles with no errors

**Step 4: Commit**

```bash
git add crates/ahandd/src/browser_init.rs crates/ahandd/src/main.rs
git commit -m "feat(ahandd): add browser_init module with Rust-native setup"
```

---

### Task 3: Add clap subcommand to ahandd main.rs

**Files:**
- Modify: `crates/ahandd/src/main.rs`

**Step 1: Refactor Args to support subcommands**

Change the existing `Args` struct to use an optional subcommand. When no subcommand is given, run the daemon (existing behavior). Add a `BrowserInit` subcommand variant.

In `crates/ahandd/src/main.rs`, replace the `Args` struct definition with:

```rust
#[derive(Parser)]
#[command(name = "ahandd", about = "AHand local execution daemon")]
struct Args {
    /// Connection mode: "ahand-cloud" (default) or "openclaw-gateway"
    #[arg(long, env = "AHAND_MODE")]
    mode: Option<String>,

    /// Cloud WebSocket URL (e.g. ws://localhost:3000/ws) - for ahand-cloud mode
    #[arg(long, env = "AHAND_URL")]
    url: Option<String>,

    /// Path to config file (TOML)
    #[arg(long, short, env = "AHAND_CONFIG")]
    config: Option<PathBuf>,

    /// Maximum number of concurrent jobs
    #[arg(long, env = "AHAND_MAX_JOBS")]
    max_jobs: Option<usize>,

    /// Directory for trace logs and run artifacts
    #[arg(long, env = "AHAND_DATA_DIR")]
    data_dir: Option<String>,

    /// Enable debug IPC server (Unix socket)
    #[arg(long, env = "AHAND_DEBUG_IPC")]
    debug_ipc: bool,

    /// Custom path for the IPC Unix socket
    #[arg(long, env = "AHAND_IPC_SOCKET")]
    ipc_socket: Option<String>,

    // OpenClaw Gateway options
    /// OpenClaw Gateway host
    #[arg(long, env = "OPENCLAW_GATEWAY_HOST")]
    gateway_host: Option<String>,

    /// OpenClaw Gateway port
    #[arg(long, env = "OPENCLAW_GATEWAY_PORT")]
    gateway_port: Option<u16>,

    /// Use TLS for OpenClaw Gateway connection
    #[arg(long, env = "OPENCLAW_GATEWAY_TLS")]
    gateway_tls: bool,

    /// OpenClaw node ID
    #[arg(long, env = "OPENCLAW_NODE_ID")]
    node_id: Option<String>,

    /// OpenClaw node display name
    #[arg(long, env = "OPENCLAW_DISPLAY_NAME")]
    display_name: Option<String>,

    /// OpenClaw Gateway authentication token
    #[arg(long, env = "OPENCLAW_GATEWAY_TOKEN")]
    gateway_token: Option<String>,

    /// OpenClaw Gateway authentication password
    #[arg(long, env = "OPENCLAW_GATEWAY_PASSWORD")]
    gateway_password: Option<String>,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Initialize browser automation dependencies
    BrowserInit {
        /// Force reinstall (clean existing installation first)
        #[arg(long)]
        force: bool,
    },
}
```

Add `use clap::Subcommand;` to the existing clap import (change `use clap::Parser;` to `use clap::{Parser, Subcommand};`).

**Step 2: Handle the subcommand early in main()**

At the start of `main()`, right after `let args = Args::parse();`, add:

```rust
// Handle subcommands that don't need daemon setup.
if let Some(cmd) = &args.command {
    match cmd {
        Cmd::BrowserInit { force } => {
            return browser_init::run(*force).await;
        }
    }
}
```

**Step 3: Verify it compiles**

Run: `cargo check -p ahandd`
Expected: compiles with no errors

**Step 4: Manual smoke test**

Run: `cargo run -p ahandd -- browser-init --help`
Expected: shows help text for browser-init subcommand

Run: `cargo run -p ahandd -- --help`
Expected: shows existing daemon options plus `browser-init` subcommand

**Step 5: Commit**

```bash
git add crates/ahandd/src/main.rs
git commit -m "feat(ahandd): add browser-init subcommand to CLI"
```

---

### Task 4: Update browser.rs warning messages

**Files:**
- Modify: `crates/ahandd/src/browser.rs`

**Step 1: Replace "ahandctl browser-init" with "ahandd browser-init"**

In `crates/ahandd/src/browser.rs`, change the three `warn!` messages in `check_prerequisites()`:

- Line 107: `"agent-browser CLI not found — run: ahandctl browser-init"` → `"agent-browser CLI not found — run: ahandd browser-init"`
- Line 118: `"daemon.js not found — run: ahandctl browser-init"` → `"daemon.js not found — run: ahandd browser-init"`
- Line 127: `"no system browser found and no Chromium installed — run: ahandctl browser-init"` → `"no system browser found and no Chromium installed — run: ahandd browser-init"`

**Step 2: Verify it compiles**

Run: `cargo check -p ahandd`

**Step 3: Commit**

```bash
git add crates/ahandd/src/browser.rs
git commit -m "fix(ahandd): update browser-init hint to use ahandd subcommand"
```

---

### Task 5: Add browser_init command to team9 Tauri layer

**Files:**
- Modify: `/Users/jiangtao/Desktop/shenjingyuan/team9/apps/client/src-tauri/src/ahand.rs`
- Modify: `/Users/jiangtao/Desktop/shenjingyuan/team9/apps/client/src-tauri/src/lib.rs`

**Step 1: Add browser_init function to ahand.rs**

Append to `ahand.rs` (before the closing of the file):

```rust
/// Run `ahandd browser-init` to install browser automation dependencies.
/// Called by the React frontend to set up browser capabilities.
pub fn browser_init(force: bool) -> Result<(), String> {
    let binary = find_binary()
        .ok_or_else(|| "aHand is not installed. Please install it first.".to_string())?;

    let mut cmd = Command::new(&binary);
    cmd.arg("browser-init");
    if force {
        cmd.arg("--force");
    }

    let status = cmd.status().map_err(|e| format!("failed to run ahandd browser-init: {e}"))?;
    if !status.success() {
        return Err("browser-init failed".to_string());
    }
    Ok(())
}

/// Check if browser automation dependencies are installed.
pub fn browser_is_ready() -> bool {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return false,
    };
    let ahand = home.join(".ahand");
    ahand.join("bin").join("agent-browser").exists()
        && ahand.join("browser").join("dist").join("daemon.js").exists()
}
```

**Step 2: Add Tauri commands in lib.rs**

Add these commands and register them:

```rust
/// Install browser automation dependencies via ahandd browser-init.
#[tauri::command]
fn ahand_browser_init(force: bool) -> Result<(), String> {
    ahand::browser_init(force)
}

/// Check if browser automation dependencies are installed.
#[tauri::command]
fn ahand_browser_is_ready() -> bool {
    ahand::browser_is_ready()
}
```

Add `ahand_browser_init` and `ahand_browser_is_ready` to the `invoke_handler` list in `tauri::generate_handler![]`.

**Step 3: Auto-detect in start()**

In `ahand.rs`, modify the `start()` function. After `write_config(...)` and before spawning the child process, add:

```rust
// Auto-install browser dependencies if not ready.
if !browser_is_ready() {
    tracing::info!("Browser dependencies not found, running browser-init...");
    let init_status = Command::new(&binary)
        .arg("browser-init")
        .status();
    if let Err(e) = init_status {
        tracing::warn!("browser-init failed: {e}");
    }
}
```

Note: `tracing` is not currently a dependency in team9 Tauri. Use `eprintln!` instead:

```rust
if !browser_is_ready() {
    eprintln!("[ahand] Browser dependencies not found, running browser-init...");
    let _ = Command::new(&binary).arg("browser-init").status();
}
```

**Step 4: Verify team9 compiles**

Run: `cd /Users/jiangtao/Desktop/shenjingyuan/team9/apps/client/src-tauri && cargo check`

**Step 5: Commit**

```bash
cd /Users/jiangtao/Desktop/shenjingyuan/team9
git add apps/client/src-tauri/src/ahand.rs apps/client/src-tauri/src/lib.rs
git commit -m "feat(client): add browser-init support via ahandd subcommand"
```

---

### Task 6: End-to-end smoke test

**Step 1: Build ahandd**

Run: `cd /Users/jiangtao/Desktop/shenjingyuan/aHand && cargo build -p ahandd`

**Step 2: Test browser-init help**

Run: `./target/debug/ahandd browser-init --help`
Expected: shows `--force` flag

**Step 3: Test browser-init runs (if network available)**

Run: `./target/debug/ahandd browser-init`
Expected: downloads agent-browser, daemon bundle, detects/installs browser, writes env.sh

**Step 4: Verify daemon mode still works**

Run: `./target/debug/ahandd --help`
Expected: shows all existing daemon flags plus `browser-init` subcommand

**Step 5: Commit any final fixes**

```bash
git add -A
git commit -m "test: verify browser-init end-to-end"
```
