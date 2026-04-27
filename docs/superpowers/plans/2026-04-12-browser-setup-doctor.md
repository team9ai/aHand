# Browser Setup Doctor & Modular Init Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor the daemon's browser setup into a reusable library module, add a `browser-doctor` diagnostic command, add `--step` flag to `browser-init` for single-step installs, and extend browser detection to include Microsoft Edge and Windows paths.

**Architecture:** Split the flat `browser_init.rs` into a `browser_setup/` directory with structured return types (`CheckReport`, `CheckStatus`, `ProgressEvent`) and a progress callback API. CLI formatting moves to a new `cli/` module. Browser detection is extracted from `browser.rs` into the shared library so it can be reused by future Tauri apps.

**Tech Stack:** Rust, tokio, clap, serde (for Tauri-compatible types), reqwest, xz2/tar

**Spec:** `docs/superpowers/specs/2026-04-12-browser-setup-doctor-design.md`

---

## File Structure

### Created

| File | Responsibility |
|------|----------------|
| `crates/ahandd/src/browser_setup/mod.rs` | Public API: `inspect_all`, `inspect`, `run_all`, `run_step`, `detect_browser`, `detect_all_browsers`. Re-exports types from `types.rs`. |
| `crates/ahandd/src/browser_setup/types.rs` | `CheckStatus`, `CheckReport`, `CheckSource`, `FixHint`, `PlatformCommand`, `ProgressEvent`, `Phase`, `DetectedBrowser`, `BrowserKind`. All derive `Serialize` so Tauri can `emit` them. |
| `crates/ahandd/src/browser_setup/node.rs` | Node.js `inspect()` + `ensure()`. Contains the download/extract logic moved from `browser_init.rs`. Uses progress callback. |
| `crates/ahandd/src/browser_setup/playwright.rs` | playwright-cli `inspect()` + `ensure()`. Contains npm invocation moved from `browser_init.rs`. Uses progress callback. |
| `crates/ahandd/src/browser_setup/browser_detect.rs` | `detect()` + `detect_all()`: platform-specific Chrome/Chromium/Edge path lookup. |
| `crates/ahandd/src/cli/mod.rs` | Module index: `pub mod browser_doctor; pub mod browser_init;` |
| `crates/ahandd/src/cli/browser_doctor.rs` | CLI adapter for `browser-doctor`: formats `CheckReport` to terminal, computes exit code. |
| `crates/ahandd/src/cli/browser_init.rs` | CLI adapter for `browser-init`: prints progress events, prints summary. |

### Modified

| File | Change |
|------|--------|
| `crates/ahandd/src/main.rs` | Replace `mod browser_init;` with `mod browser_setup; mod cli;`. Add `BrowserDoctor` subcommand. Add `--step` arg to `BrowserInit`. Route both to `cli::browser_doctor::run()` / `cli::browser_init::run()`. |
| `crates/ahandd/src/browser.rs` | Replace the inline `resolve_executable_path()` body with a call to `browser_setup::detect_browser()`. |

### Deleted

| File | Reason |
|------|--------|
| `crates/ahandd/src/browser_init.rs` | Replaced by `browser_setup/` directory. |

---

### Task 1: Create `browser_setup/types.rs` with structured types

**Goal:** Define all the public types (`CheckStatus`, `CheckReport`, `ProgressEvent`, etc.) with `Serialize` derives so they're usable from both CLI and Tauri.

**Files:**
- Create: `crates/ahandd/src/browser_setup/types.rs`
- Create: `crates/ahandd/src/browser_setup/mod.rs` (stub with `pub mod types;`)
- Modify: `crates/ahandd/src/main.rs` (add `mod browser_setup;` alongside existing `mod browser_init;`)

**Acceptance Criteria:**
- [ ] All 9 types from the spec (`CheckStatus`, `CheckSource`, `CheckReport`, `FixHint`, `PlatformCommand`, `ProgressEvent`, `Phase`, `DetectedBrowser`, `BrowserKind`) exist with `Serialize` derives
- [ ] Enums use `#[serde(tag = "kind", rename_all = "snake_case")]` where applicable
- [ ] Unit test verifies serialization produces the documented JSON shape

**Verify:** `cargo test -p ahandd browser_setup::types` → passes

**Steps:**

- [ ] **Step 1: Create `crates/ahandd/src/browser_setup/mod.rs`**

```rust
//! Browser automation setup: checks, installs, and browser detection.
//!
//! This module is designed to be reusable from both the `ahandd` CLI and
//! future Tauri-based frontends. All public types derive `Serialize` so they
//! can be emitted directly to a JavaScript frontend without transformation.
//!
//! The core API returns structured data; display concerns (terminal output,
//! GUI rendering) live in adapter modules (`crate::cli::browser_doctor`,
//! `crate::cli::browser_init`).

pub mod types;

pub use types::*;
```

- [ ] **Step 2: Create `crates/ahandd/src/browser_setup/types.rs`**

```rust
use std::path::PathBuf;

use serde::Serialize;

/// Status of a single setup check.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckStatus {
    /// Component is installed and meets requirements.
    Ok {
        version: String,
        path: PathBuf,
        source: CheckSource,
    },
    /// Component is not installed.
    Missing,
    /// Component is installed but version is too old.
    Outdated {
        current: String,
        required: String,
        path: PathBuf,
    },
    /// Applies to the browser check: none of the known browsers were found.
    NoneDetected { tried: Vec<String> },
}

/// Where a detected component comes from.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckSource {
    /// Installed by ahandd under ~/.ahand/...
    Managed,
    /// System-wide install (e.g. Chrome under /Applications).
    System,
    /// OS-shipped default (e.g. Edge on Windows).
    Preinstalled,
}

/// Full report for a single check, including any fix hint.
#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    /// Internal name: "node", "playwright", "browser".
    pub name: &'static str,
    /// Human-readable label: "Node.js", "playwright-cli", "System Browser".
    pub label: &'static str,
    pub status: CheckStatus,
    pub fix_hint: Option<FixHint>,
}

/// How to fix a failed check.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FixHint {
    /// Run `ahandd browser-init --step <name>`.
    RunStep { command: String },
    /// Manual per-platform commands the user must run themselves.
    ManualCommand {
        platform_commands: Vec<PlatformCommand>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct PlatformCommand {
    pub platform: &'static str, // "macOS" / "Linux" / "Windows"
    pub command: String,
}

/// Progress update emitted during install operations.
#[derive(Debug, Clone, Serialize)]
pub struct ProgressEvent {
    /// Which step is reporting: "node" / "playwright".
    pub step: &'static str,
    pub phase: Phase,
    pub message: String,
    /// Percent complete (0-100), only populated for measurable operations.
    pub percent: Option<u8>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Starting,
    Downloading,
    Extracting,
    Installing,
    Verifying,
    Done,
}

/// A detected system browser.
#[derive(Debug, Clone, Serialize)]
pub struct DetectedBrowser {
    pub name: String,
    pub path: PathBuf,
    pub kind: BrowserKind,
    pub source: CheckSource,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserKind {
    Chrome,
    Chromium,
    Edge,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn check_status_ok_serializes_with_tag() {
        let status = CheckStatus::Ok {
            version: "24.13.0".into(),
            path: PathBuf::from("/foo/node"),
            source: CheckSource::Managed,
        };
        let actual = serde_json::to_value(&status).unwrap();
        assert_eq!(
            actual,
            json!({
                "kind": "ok",
                "version": "24.13.0",
                "path": "/foo/node",
                "source": "managed"
            })
        );
    }

    #[test]
    fn check_status_missing_serializes_with_tag_only() {
        let status = CheckStatus::Missing;
        let actual = serde_json::to_value(&status).unwrap();
        assert_eq!(actual, json!({ "kind": "missing" }));
    }

    #[test]
    fn fix_hint_run_step_serializes() {
        let hint = FixHint::RunStep {
            command: "ahandd browser-init --step node".into(),
        };
        let actual = serde_json::to_value(&hint).unwrap();
        assert_eq!(
            actual,
            json!({
                "kind": "run_step",
                "command": "ahandd browser-init --step node"
            })
        );
    }

    #[test]
    fn progress_event_serializes_with_snake_case_phase() {
        let event = ProgressEvent {
            step: "node",
            phase: Phase::Downloading,
            message: "Downloading tarball".into(),
            percent: Some(42),
        };
        let actual = serde_json::to_value(&event).unwrap();
        assert_eq!(
            actual,
            json!({
                "step": "node",
                "phase": "downloading",
                "message": "Downloading tarball",
                "percent": 42
            })
        );
    }

    #[test]
    fn browser_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(&BrowserKind::Edge).unwrap(),
            json!("edge")
        );
    }
}
```

- [ ] **Step 3: Register the new module in `main.rs`**

In `crates/ahandd/src/main.rs`, add `mod browser_setup;` in the module list (keep the existing `mod browser_init;` for now — it'll be deleted in Task 4):

```rust
mod ahand_client;
mod approval;
mod browser;
mod browser_init;
mod browser_setup;
mod config;
```

- [ ] **Step 4: Verify compilation and tests**

Run: `cargo test -p ahandd browser_setup::types`
Expected: 5 tests passing

- [ ] **Step 5: Commit**

```bash
git add crates/ahandd/src/browser_setup/ crates/ahandd/src/main.rs
git commit -m "feat(ahandd): add browser_setup types module"
```

---

### Task 2: Implement `browser_setup/browser_detect.rs`

**Goal:** Extract browser detection from `src/browser.rs`, add Edge support, add full Windows paths, and expose `detect()`/`detect_all()` as library functions with unit tests.

**Files:**
- Create: `crates/ahandd/src/browser_setup/browser_detect.rs`
- Modify: `crates/ahandd/src/browser_setup/mod.rs` (add `pub mod browser_detect;`)

**Acceptance Criteria:**
- [ ] `detect(config_override: Option<&str>)` returns the first available browser per platform priority
- [ ] `detect_all()` returns all detected browsers
- [ ] Detection order matches the spec (Chrome → Chromium → Edge)
- [ ] Windows paths included (Chrome x64/x86, Edge x86/x64)
- [ ] `config_override` wins over auto-detection
- [ ] Unit tests cover each platform branch using a mock "path exists" function

**Verify:** `cargo test -p ahandd browser_setup::browser_detect` → passes

**Steps:**

- [ ] **Step 1: Create `crates/ahandd/src/browser_setup/browser_detect.rs`**

```rust
use std::path::{Path, PathBuf};

use super::types::{BrowserKind, CheckSource, DetectedBrowser};

/// A candidate browser with its kind, path, display name, and source.
struct Candidate {
    kind: BrowserKind,
    path: &'static str,
    name: &'static str,
    source: CheckSource,
}

/// Detect a system browser. Respects `config_override` first, then falls back
/// to auto-detection with the platform-specific priority order.
pub fn detect(config_override: Option<&str>) -> Option<DetectedBrowser> {
    detect_with(config_override, &|p| Path::new(p).exists())
}

/// Detect all system browsers currently installed.
pub fn detect_all() -> Vec<DetectedBrowser> {
    detect_all_with(&|p| Path::new(p).exists())
}

fn detect_with(
    config_override: Option<&str>,
    exists: &dyn Fn(&str) -> bool,
) -> Option<DetectedBrowser> {
    if let Some(path) = config_override {
        if exists(path) {
            return Some(DetectedBrowser {
                name: "Configured Browser".into(),
                path: PathBuf::from(path),
                kind: BrowserKind::Chrome, // conservative default for config override
                source: CheckSource::System,
            });
        }
    }

    for c in candidates() {
        if exists(c.path) {
            return Some(DetectedBrowser {
                name: c.name.into(),
                path: PathBuf::from(c.path),
                kind: c.kind.clone(),
                source: c.source.clone(),
            });
        }
    }
    None
}

fn detect_all_with(exists: &dyn Fn(&str) -> bool) -> Vec<DetectedBrowser> {
    candidates()
        .into_iter()
        .filter(|c| exists(c.path))
        .map(|c| DetectedBrowser {
            name: c.name.into(),
            path: PathBuf::from(c.path),
            kind: c.kind,
            source: c.source,
        })
        .collect()
}

/// Human-readable list of browser names tried during detection, for error messages.
pub fn tried_browsers() -> Vec<String> {
    vec!["Chrome".into(), "Chromium".into(), "Edge".into()]
}

fn candidates() -> Vec<Candidate> {
    #[cfg(target_os = "macos")]
    {
        vec![
            Candidate {
                kind: BrowserKind::Chrome,
                path: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                name: "Google Chrome",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Chrome,
                path: "/Applications/Google Chrome Dev.app/Contents/MacOS/Google Chrome Dev",
                name: "Google Chrome Dev",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Chrome,
                path: "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
                name: "Google Chrome Canary",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Chromium,
                path: "/Applications/Chromium.app/Contents/MacOS/Chromium",
                name: "Chromium",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Edge,
                path: "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
                name: "Microsoft Edge",
                source: CheckSource::System,
            },
        ]
    }

    #[cfg(target_os = "linux")]
    {
        vec![
            Candidate {
                kind: BrowserKind::Chrome,
                path: "/usr/bin/google-chrome-stable",
                name: "Google Chrome",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Chrome,
                path: "/usr/bin/google-chrome",
                name: "Google Chrome",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Chromium,
                path: "/usr/bin/chromium",
                name: "Chromium",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Chromium,
                path: "/usr/bin/chromium-browser",
                name: "Chromium",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Edge,
                path: "/usr/bin/microsoft-edge-stable",
                name: "Microsoft Edge",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Edge,
                path: "/usr/bin/microsoft-edge",
                name: "Microsoft Edge",
                source: CheckSource::System,
            },
        ]
    }

    #[cfg(target_os = "windows")]
    {
        vec![
            Candidate {
                kind: BrowserKind::Chrome,
                path: r"C:\Program Files\Google\Chrome\Application\chrome.exe",
                name: "Google Chrome",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Chrome,
                path: r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
                name: "Google Chrome",
                source: CheckSource::System,
            },
            Candidate {
                kind: BrowserKind::Edge,
                path: r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
                name: "Microsoft Edge",
                source: CheckSource::Preinstalled,
            },
            Candidate {
                kind: BrowserKind::Edge,
                path: r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
                name: "Microsoft Edge",
                source: CheckSource::Preinstalled,
            },
        ]
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exists_any(_: &str) -> bool {
        true
    }
    fn exists_none(_: &str) -> bool {
        false
    }

    #[test]
    fn detect_returns_first_candidate_when_all_exist() {
        let result = detect_with(None, &exists_any);
        assert!(result.is_some(), "expected some browser");
        // The first candidate varies by platform. Just verify we got something.
    }

    #[test]
    fn detect_returns_none_when_nothing_exists() {
        let result = detect_with(None, &exists_none);
        assert!(result.is_none());
    }

    #[test]
    fn detect_config_override_takes_priority_when_path_exists() {
        let result = detect_with(Some("/any/path"), &exists_any);
        assert!(result.is_some());
        let browser = result.unwrap();
        assert_eq!(browser.path, PathBuf::from("/any/path"));
        assert_eq!(browser.name, "Configured Browser");
    }

    #[test]
    fn detect_config_override_falls_back_when_path_missing() {
        // Override path doesn't exist but other candidates do.
        let exists = |p: &str| p != "/missing/override";
        let result = detect_with(Some("/missing/override"), &exists);
        assert!(result.is_some());
        assert_ne!(result.unwrap().path, PathBuf::from("/missing/override"));
    }

    #[test]
    fn detect_all_returns_multiple_when_present() {
        let all = detect_all_with(&exists_any);
        // At least one candidate exists on every platform we build for.
        assert!(!all.is_empty());
    }

    #[test]
    fn detect_all_returns_empty_when_none_exist() {
        let all = detect_all_with(&exists_none);
        assert!(all.is_empty());
    }

    #[test]
    fn tried_browsers_lists_expected_names() {
        let tried = tried_browsers();
        assert_eq!(tried, vec!["Chrome", "Chromium", "Edge"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_priority_chrome_before_edge() {
        // Only Edge exists.
        let exists = |p: &str| p.contains("Microsoft Edge");
        let result = detect_with(None, &exists);
        let browser = result.expect("expected edge");
        assert!(matches!(browser.kind, BrowserKind::Edge));

        // Both Chrome and Edge exist — Chrome wins.
        let exists_both = |p: &str| p.contains("Google Chrome") || p.contains("Microsoft Edge");
        let both = detect_with(None, &exists_both).unwrap();
        assert!(matches!(both.kind, BrowserKind::Chrome));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_edge_marked_preinstalled() {
        let exists_edge_only = |p: &str| p.contains("Edge");
        let browser = detect_with(None, &exists_edge_only).unwrap();
        assert!(matches!(browser.source, CheckSource::Preinstalled));
    }
}
```

- [ ] **Step 2: Wire the module into `browser_setup/mod.rs`**

In `crates/ahandd/src/browser_setup/mod.rs`, add the module and re-export functions:

```rust
//! Browser automation setup: checks, installs, and browser detection.

pub mod browser_detect;
pub mod types;

pub use browser_detect::{detect as detect_browser, detect_all as detect_all_browsers, tried_browsers};
pub use types::*;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ahandd browser_setup::browser_detect`
Expected: All tests pass (exact count depends on target platform; at least 6 pass everywhere plus platform-specific)

- [ ] **Step 4: Commit**

```bash
git add crates/ahandd/src/browser_setup/browser_detect.rs crates/ahandd/src/browser_setup/mod.rs
git commit -m "feat(ahandd): extract browser detection with Edge/Windows support"
```

---

### Task 3: Implement `browser_setup/node.rs` with inspect and ensure

**Goal:** Move Node.js download/install logic from `browser_init.rs` into `browser_setup/node.rs`, add the `inspect()` function that returns a `CheckReport`, and replace `println!` calls with progress callback events.

**Files:**
- Create: `crates/ahandd/src/browser_setup/node.rs`
- Modify: `crates/ahandd/src/browser_setup/mod.rs` (add `pub mod node;`)

**Acceptance Criteria:**
- [ ] `inspect()` returns `CheckReport` with status `Ok` / `Missing` / `Outdated` based on `~/.ahand/node/bin/node`
- [ ] `ensure(force, progress_cb)` downloads+installs Node.js, reporting progress via callback
- [ ] No `println!` calls in this module
- [ ] Callback receives events for `Starting`, `Downloading`, `Extracting`, `Verifying`, `Done` phases
- [ ] Unit test: `inspect()` returns `Missing` when binary doesn't exist

**Verify:** `cargo test -p ahandd browser_setup::node::tests::inspect_returns_missing_when_node_absent` → passes (and `cargo check -p ahandd` succeeds)

**Steps:**

- [ ] **Step 1: Create `crates/ahandd/src/browser_setup/node.rs`**

```rust
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::types::{CheckReport, CheckSource, CheckStatus, FixHint, Phase, ProgressEvent};

pub const NODE_MIN_VERSION: u32 = 20;
pub const NODE_LTS_VERSION: &str = "24.13.0";

pub struct Dirs {
    pub ahand: PathBuf,
    pub node: PathBuf,
}

impl Dirs {
    pub fn new() -> Result<Self> {
        let home = dirs::home_dir().context("cannot determine home directory")?;
        let ahand = home.join(".ahand");
        Ok(Self {
            node: ahand.join("node"),
            ahand,
        })
    }
}

fn local_node_bin() -> Result<PathBuf> {
    let dirs = Dirs::new()?;
    Ok(dirs.node.join("bin").join("node"))
}

/// Read-only check: report current Node.js status.
pub async fn inspect() -> CheckReport {
    let report = async {
        let bin = local_node_bin()?;
        if !bin.exists() {
            return Ok::<CheckReport, anyhow::Error>(CheckReport {
                name: "node",
                label: "Node.js",
                status: CheckStatus::Missing,
                fix_hint: Some(FixHint::RunStep {
                    command: "ahandd browser-init --step node".into(),
                }),
            });
        }
        match read_node_major_version(&bin).await {
            Some(ver) if ver >= NODE_MIN_VERSION => Ok(CheckReport {
                name: "node",
                label: "Node.js",
                status: CheckStatus::Ok {
                    version: format!("v{ver}.x"),
                    path: bin,
                    source: CheckSource::Managed,
                },
                fix_hint: None,
            }),
            Some(ver) => Ok(CheckReport {
                name: "node",
                label: "Node.js",
                status: CheckStatus::Outdated {
                    current: format!("v{ver}"),
                    required: format!("v{NODE_MIN_VERSION}"),
                    path: bin,
                },
                fix_hint: Some(FixHint::RunStep {
                    command: "ahandd browser-init --force --step node".into(),
                }),
            }),
            None => Ok(CheckReport {
                name: "node",
                label: "Node.js",
                status: CheckStatus::Missing,
                fix_hint: Some(FixHint::RunStep {
                    command: "ahandd browser-init --force --step node".into(),
                }),
            }),
        }
    }
    .await;

    report.unwrap_or_else(|_| CheckReport {
        name: "node",
        label: "Node.js",
        status: CheckStatus::Missing,
        fix_hint: Some(FixHint::RunStep {
            command: "ahandd browser-init --step node".into(),
        }),
    })
}

/// Ensure Node.js is installed. If `force`, reinstall even if present.
pub async fn ensure(
    force: bool,
    progress: &(dyn Fn(ProgressEvent) + Send + Sync),
) -> Result<CheckReport> {
    let dirs = Dirs::new()?;
    let local_node = dirs.node.join("bin").join("node");

    if !force && local_node.exists() {
        if let Some(ver) = read_node_major_version(&local_node).await {
            if ver >= NODE_MIN_VERSION {
                emit(
                    progress,
                    Phase::Done,
                    format!("Node.js v{ver}.x already installed at {}", dirs.node.display()),
                );
                return Ok(inspect().await);
            }
        }
    }

    // Remove the old installation (whether --force was set or version was too low)
    // to avoid stale files from a previous version mixing with the new one.
    if dirs.node.exists() {
        let _ = std::fs::remove_dir_all(&dirs.node);
    }

    emit(
        progress,
        Phase::Starting,
        format!("Installing Node.js v{NODE_LTS_VERSION} to {}", dirs.node.display()),
    );

    install_node(&dirs, progress).await.context(
        "Failed to install Node.js. Check your network connection and retry, \
         or install Node.js >= 20 manually (e.g. `brew install node`).",
    )?;

    if !local_node.exists() {
        anyhow::bail!(
            "Node.js installation completed but binary not found at {}.",
            local_node.display()
        );
    }

    emit(
        progress,
        Phase::Done,
        format!("Node.js v{NODE_LTS_VERSION} ready at {}", dirs.node.display()),
    );

    Ok(inspect().await)
}

async fn read_node_major_version(node_bin: &Path) -> Option<u32> {
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

async fn install_node(
    dirs: &Dirs,
    progress: &(dyn Fn(ProgressEvent) + Send + Sync),
) -> Result<()> {
    let (os, arch) = platform_info();
    let tarball = format!("node-v{NODE_LTS_VERSION}-{os}-{arch}.tar.xz");
    let url = format!("https://nodejs.org/dist/v{NODE_LTS_VERSION}/{tarball}");

    emit(
        progress,
        Phase::Downloading,
        format!("Downloading {tarball}"),
    );

    let bytes = download_bytes(&url, progress).await.context(format!(
        "Failed to download Node.js from {url} — check your network connection"
    ))?;

    std::fs::create_dir_all(&dirs.node).context(format!(
        "Failed to create directory {}: permission denied or disk full",
        dirs.node.display()
    ))?;

    emit(
        progress,
        Phase::Extracting,
        format!("Extracting Node.js archive"),
    );

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
        if !dest.starts_with(&dirs.node) {
            continue; // skip entries that would escape extraction root
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry.unpack(&dest).context(format!(
            "Failed to extract {} — disk may be full",
            dest.display()
        ))?;
    }

    emit(
        progress,
        Phase::Verifying,
        format!("Verifying Node.js installation"),
    );

    Ok(())
}

async fn download_bytes(
    url: &str,
    _progress: &(dyn Fn(ProgressEvent) + Send + Sync),
) -> Result<Vec<u8>> {
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

    // Note: we could stream with progress_with_cb here in the future;
    // for now just buffer the whole thing. The spec allows percent to be
    // None when we don't have measurable progress.
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

fn emit(progress: &(dyn Fn(ProgressEvent) + Send + Sync), phase: Phase, message: String) {
    progress(ProgressEvent {
        step: "node",
        phase,
        message,
        percent: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn inspect_returns_missing_when_node_absent() {
        // This test is environment-dependent: it checks that if the user's
        // ~/.ahand/node/bin/node does NOT exist, inspect() returns Missing.
        // Skip if it happens to exist on the test machine.
        let bin = local_node_bin().unwrap();
        if bin.exists() {
            eprintln!("skipping: {} already exists", bin.display());
            return;
        }
        let report = inspect().await;
        assert_eq!(report.name, "node");
        assert_eq!(report.label, "Node.js");
        assert!(matches!(report.status, CheckStatus::Missing));
        assert!(matches!(report.fix_hint, Some(FixHint::RunStep { .. })));
    }
}
```

- [ ] **Step 2: Add `pub mod node;` to `browser_setup/mod.rs`**

In `crates/ahandd/src/browser_setup/mod.rs`:

```rust
//! Browser automation setup: checks, installs, and browser detection.

pub mod browser_detect;
pub mod node;
pub mod types;

pub use browser_detect::{detect as detect_browser, detect_all as detect_all_browsers, tried_browsers};
pub use types::*;
```

- [ ] **Step 3: Verify compilation and test**

Run: `cargo test -p ahandd browser_setup::node`
Expected: Test passes (or is skipped gracefully if Node is already installed locally)

- [ ] **Step 4: Commit**

```bash
git add crates/ahandd/src/browser_setup/node.rs crates/ahandd/src/browser_setup/mod.rs
git commit -m "feat(ahandd): add browser_setup::node with inspect and ensure"
```

---

### Task 4: Implement `browser_setup/playwright.rs` and orchestration API

**Goal:** Move playwright-cli install logic into `browser_setup/playwright.rs`, add `inspect()` / `ensure()`, implement the top-level `run_all` / `run_step` / `inspect_all` orchestration functions, and delete the old `browser_init.rs`.

**Files:**
- Create: `crates/ahandd/src/browser_setup/playwright.rs`
- Modify: `crates/ahandd/src/browser_setup/mod.rs` (add `pub mod playwright;` and orchestration functions)
- Delete: `crates/ahandd/src/browser_init.rs`
- Modify: `crates/ahandd/src/main.rs` (remove `mod browser_init;` and route `BrowserInit` subcommand through `cli::browser_init` — will be wired in Task 6; temporarily call `browser_setup::run_all` directly)

**Acceptance Criteria:**
- [ ] `playwright::inspect()` reports `Ok` / `Missing` for playwright-cli
- [ ] `playwright::ensure(force, progress)` installs via npm, reporting progress
- [ ] `run_all(force, progress)` runs `node::ensure` then `playwright::ensure`, returns `Vec<CheckReport>`
- [ ] `run_step(name, force, progress)` routes to `node::ensure` or `playwright::ensure`; on `"playwright"` with Node missing, returns an error with a clear message
- [ ] `inspect_all()` returns three `CheckReport`s (node, playwright, browser) without mutations
- [ ] `inspect(name)` returns `Option<CheckReport>` for a single check
- [ ] `browser_init.rs` is deleted
- [ ] `cargo check -p ahandd` succeeds

**Verify:** `cargo test -p ahandd browser_setup` → passes; `cargo check -p ahandd` → no errors

**Steps:**

- [ ] **Step 1: Create `crates/ahandd/src/browser_setup/playwright.rs`**

```rust
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};

use super::node::Dirs;
use super::types::{CheckReport, CheckSource, CheckStatus, FixHint, Phase, ProgressEvent};

pub const PLAYWRIGHT_CLI_VERSION: &str = "0.1.1";

fn cli_path() -> Result<PathBuf> {
    let dirs = Dirs::new()?;
    Ok(dirs.node.join("bin").join("playwright-cli"))
}

/// Read-only check: report current playwright-cli status.
pub async fn inspect() -> CheckReport {
    let Ok(cli) = cli_path() else {
        return missing_report();
    };

    if !cli.exists() {
        return missing_report();
    }

    let output = tokio::process::Command::new(&cli)
        .arg("--version")
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
            CheckReport {
                name: "playwright",
                label: "playwright-cli",
                status: CheckStatus::Ok {
                    version,
                    path: cli,
                    source: CheckSource::Managed,
                },
                fix_hint: None,
            }
        }
        _ => missing_report(),
    }
}

fn missing_report() -> CheckReport {
    CheckReport {
        name: "playwright",
        label: "playwright-cli",
        status: CheckStatus::Missing,
        fix_hint: Some(FixHint::RunStep {
            command: "ahandd browser-init --step playwright".into(),
        }),
    }
}

/// Ensure playwright-cli is installed at the pinned version.
/// If `force`, uninstall first and reinstall.
pub async fn ensure(
    force: bool,
    progress: &(dyn Fn(ProgressEvent) + Send + Sync),
) -> Result<CheckReport> {
    let dirs = Dirs::new()?;
    let npm = dirs.node.join("bin").join("npm");
    if !npm.exists() {
        anyhow::bail!(
            "npm not found at {} — install Node.js first (`ahandd browser-init --step node`)",
            npm.display()
        );
    }
    let cli = cli_path()?;
    let prefix = dirs.node.to_string_lossy().to_string();

    if force && cli.exists() {
        emit(progress, Phase::Starting, "Uninstalling existing playwright-cli".into());
        let _ = tokio::process::Command::new(&npm)
            .args(["uninstall", "-g", "--prefix", &prefix, "@playwright/cli"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }

    // Check cache (skip if unchanged and not forced)
    if !force && cli.exists() {
        if let Ok(out) = tokio::process::Command::new(&cli).arg("--version").output().await {
            if out.status.success() {
                let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
                emit(progress, Phase::Done, format!("playwright-cli {ver} already installed"));
                return Ok(inspect().await);
            }
        }
    }

    emit(
        progress,
        Phase::Installing,
        format!("Installing @playwright/cli@{PLAYWRIGHT_CLI_VERSION}"),
    );

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
            "Failed to install @playwright/cli@{PLAYWRIGHT_CLI_VERSION} (exit {}):\n{}",
            output.status.code().unwrap_or(-1),
            stderr,
        );
    }

    emit(progress, Phase::Verifying, "Verifying playwright-cli".into());

    if !cli.exists() {
        anyhow::bail!(
            "playwright-cli was installed but binary not found at {}",
            cli.display()
        );
    }

    let version = tokio::process::Command::new(&cli)
        .arg("--version")
        .output()
        .await
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "installed".to_string());

    emit(progress, Phase::Done, format!("playwright-cli {version} ready"));

    Ok(inspect().await)
}

fn emit(progress: &(dyn Fn(ProgressEvent) + Send + Sync), phase: Phase, message: String) {
    progress(ProgressEvent {
        step: "playwright",
        phase,
        message,
        percent: None,
    });
}
```

- [ ] **Step 2: Add the `browser_check` helper and orchestration to `browser_setup/mod.rs`**

Replace `crates/ahandd/src/browser_setup/mod.rs` with:

```rust
//! Browser automation setup: checks, installs, and browser detection.
//!
//! Public API:
//! - `inspect_all()`, `inspect(name)` — read-only diagnostic
//! - `run_all(force, progress)` — install everything (or refresh)
//! - `run_step(name, force, progress)` — install a single component
//! - `detect_browser(config_override)`, `detect_all_browsers()` — browser detection

use anyhow::{Result, bail};

pub mod browser_detect;
pub mod node;
pub mod playwright;
pub mod types;

pub use browser_detect::{detect as detect_browser, detect_all as detect_all_browsers, tried_browsers};
pub use types::*;

/// Inspect all browser setup components. Read-only; never modifies anything.
pub async fn inspect_all() -> Vec<CheckReport> {
    vec![
        node::inspect().await,
        playwright::inspect().await,
        inspect_browser(),
    ]
}

/// Inspect a single component by name.
pub async fn inspect(name: &str) -> Option<CheckReport> {
    match name {
        "node" => Some(node::inspect().await),
        "playwright" => Some(playwright::inspect().await),
        "browser" => Some(inspect_browser()),
        _ => None,
    }
}

/// Run all install steps. `force` reinstalls even if already present.
pub async fn run_all(
    force: bool,
    progress: impl Fn(ProgressEvent) + Send + Sync + 'static,
) -> Result<Vec<CheckReport>> {
    let progress_ref: &(dyn Fn(ProgressEvent) + Send + Sync) = &progress;
    let node_report = node::ensure(force, progress_ref).await?;
    let playwright_report = playwright::ensure(force, progress_ref).await?;
    let browser_report = inspect_browser();
    Ok(vec![node_report, playwright_report, browser_report])
}

/// Run a single install step. Valid names: `node`, `playwright`.
/// Returns an error for `playwright` if Node is not already installed.
pub async fn run_step(
    name: &str,
    force: bool,
    progress: impl Fn(ProgressEvent) + Send + Sync + 'static,
) -> Result<CheckReport> {
    let progress_ref: &(dyn Fn(ProgressEvent) + Send + Sync) = &progress;
    match name {
        "node" => node::ensure(force, progress_ref).await,
        "playwright" => {
            let node_status = node::inspect().await;
            if !matches!(node_status.status, CheckStatus::Ok { .. }) {
                bail!(
                    "playwright step requires node to be installed first. \
                     Run `ahandd browser-init --step node` first, or \
                     `ahandd browser-init` for all steps."
                );
            }
            playwright::ensure(force, progress_ref).await
        }
        other => bail!("unknown step `{other}`. Valid steps: node, playwright"),
    }
}

fn inspect_browser() -> CheckReport {
    match detect_browser(None) {
        Some(browser) => {
            let version = String::new(); // no cheap way to query version
            CheckReport {
                name: "browser",
                label: "System Browser",
                status: CheckStatus::Ok {
                    version,
                    path: browser.path,
                    source: browser.source,
                },
                fix_hint: None,
            }
        }
        None => CheckReport {
            name: "browser",
            label: "System Browser",
            status: CheckStatus::NoneDetected {
                tried: tried_browsers(),
            },
            fix_hint: Some(FixHint::ManualCommand {
                platform_commands: vec![
                    PlatformCommand {
                        platform: "macOS",
                        command: "brew install --cask google-chrome".into(),
                    },
                    PlatformCommand {
                        platform: "Linux",
                        command: "sudo apt install chromium-browser (or microsoft-edge-stable)".into(),
                    },
                    PlatformCommand {
                        platform: "Windows",
                        command: "Edge should be preinstalled — please report".into(),
                    },
                ],
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_step_rejects_unknown_name() {
        let progress = |_: ProgressEvent| {};
        let result = run_step("unknown", false, progress).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown step"));
    }

    #[tokio::test]
    async fn inspect_all_returns_three_reports() {
        let reports = inspect_all().await;
        assert_eq!(reports.len(), 3);
        assert_eq!(reports[0].name, "node");
        assert_eq!(reports[1].name, "playwright");
        assert_eq!(reports[2].name, "browser");
    }

    #[tokio::test]
    async fn inspect_by_name() {
        assert!(inspect("node").await.is_some());
        assert!(inspect("playwright").await.is_some());
        assert!(inspect("browser").await.is_some());
        assert!(inspect("nothing").await.is_none());
    }
}
```

- [ ] **Step 3: Delete the old `browser_init.rs`**

```bash
rm crates/ahandd/src/browser_init.rs
```

- [ ] **Step 4: Update `main.rs` to route `BrowserInit` to the new module (temporary wiring)**

In `crates/ahandd/src/main.rs`:

1. Remove `mod browser_init;` line
2. Add `--step` flag to the `BrowserInit` variant:
   ```rust
   BrowserInit {
       /// Force reinstall (clean existing installation first)
       #[arg(long)]
       force: bool,
       /// Run only a single step: node or playwright
       #[arg(long)]
       step: Option<String>,
   },
   ```
3. Replace the match arm body to call `browser_setup::run_all` or `run_step`:
   ```rust
   Cmd::BrowserInit { force, step } => {
       let progress = |event: browser_setup::ProgressEvent| {
           println!("  [{}] {}", event.step, event.message);
       };
       return match step {
           Some(s) => {
               browser_setup::run_step(s, *force, progress).await?;
               Ok(())
           }
           None => {
               browser_setup::run_all(*force, progress).await?;
               Ok(())
           }
       };
   }
   ```

Note: This is temporary wiring — Task 6 will replace this inline closure with a call to `cli::browser_init::run()` for nicer formatting. For now, the goal is just to keep the build green.

- [ ] **Step 5: Verify compilation and tests**

Run: `cargo check -p ahandd`
Expected: Compiles with no errors

Run: `cargo test -p ahandd browser_setup`
Expected: All new tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/ahandd/src/browser_setup/ crates/ahandd/src/main.rs
git rm crates/ahandd/src/browser_init.rs
git commit -m "feat(ahandd): add playwright module and orchestration, delete browser_init"
```

---

### Task 5: Replace `browser.rs::resolve_executable_path` with shared helper

**Goal:** Delete the duplicated browser detection logic in `src/browser.rs` and call `browser_setup::detect_browser()` instead. This is now safe because Task 2 shipped the shared detection.

**Files:**
- Modify: `crates/ahandd/src/browser.rs:497-526` (replace the entire `resolve_executable_path` body)

**Acceptance Criteria:**
- [ ] `BrowserManager::resolve_executable_path()` is a one-line call to `browser_setup::detect_browser()`
- [ ] The inline platform-specific match blocks are deleted
- [ ] Config override still takes priority (handled by `detect_browser`)
- [ ] `cargo test -p ahandd` passes with no regressions

**Verify:** `cargo test -p ahandd` → passes; `cargo check -p ahandd` → no warnings

**Steps:**

- [ ] **Step 1: Replace `resolve_executable_path` in `crates/ahandd/src/browser.rs`**

Change the existing function (around line 497) from:

```rust
    /// Resolve browser executable: config > system Chrome auto-detect.
    fn resolve_executable_path(&self) -> Option<String> {
        if let Some(path) = &self.config.executable_path {
            return Some(path.clone());
        }

        #[cfg(target_os = "macos")]
        {
            for candidate in &[
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                "/Applications/Google Chrome Dev.app/Contents/MacOS/Google Chrome Dev",
                "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
                "/Applications/Chromium.app/Contents/MacOS/Chromium",
            ] {
                if std::path::Path::new(candidate).exists() {
                    return Some(candidate.to_string());
                }
            }
        }

        #[cfg(target_os = "linux")]
        {
            for candidate in &["/usr/bin/google-chrome", "/usr/bin/google-chrome-stable"] {
                if std::path::Path::new(candidate).exists() {
                    return Some(candidate.to_string());
                }
            }
        }

        None
    }
```

to:

```rust
    /// Resolve browser executable: config > system Chrome/Edge auto-detect.
    fn resolve_executable_path(&self) -> Option<String> {
        crate::browser_setup::detect_browser(self.config.executable_path.as_deref())
            .map(|b| b.path.to_string_lossy().into_owned())
    }
```

- [ ] **Step 2: Verify build and tests**

Run: `cargo test -p ahandd`
Expected: All tests pass (including existing browser.rs tests, if any)

- [ ] **Step 3: Commit**

```bash
git add crates/ahandd/src/browser.rs
git commit -m "refactor(ahandd): use shared browser detection in BrowserManager"
```

---

### Task 6: CLI adapters for `browser-doctor` and `browser-init`

**Goal:** Create the `cli/` module with terminal formatters that turn `CheckReport` / `ProgressEvent` into pretty output. Wire both subcommands through it.

**Files:**
- Create: `crates/ahandd/src/cli/mod.rs`
- Create: `crates/ahandd/src/cli/browser_doctor.rs`
- Create: `crates/ahandd/src/cli/browser_init.rs`
- Modify: `crates/ahandd/src/main.rs` (add `mod cli;`, add `BrowserDoctor` variant, route `BrowserInit` through `cli::browser_init::run`)

**Acceptance Criteria:**
- [ ] `ahandd browser-doctor` prints all three checks with `[✓]` or `[✗]`, fix hints, and exits 0 if all pass / 1 otherwise
- [ ] `ahandd browser-init` prints progress events as they arrive and a summary at the end
- [ ] `ahandd browser-init --step node` runs only the node step
- [ ] `ahandd browser-init --step playwright` errors with a clear message if Node is missing
- [ ] `ahandd browser-init --force --step playwright` reinstalls only playwright-cli

**Verify:** Manual smoke tests (below); `cargo check -p ahandd` → no errors

**Steps:**

- [ ] **Step 1: Create `crates/ahandd/src/cli/mod.rs`**

```rust
//! CLI adapters that format browser_setup output for terminal display.
//!
//! The core logic lives in `crate::browser_setup`. These modules add
//! presentation — formatting, colors, progress bars, exit codes.

pub mod browser_doctor;
pub mod browser_init;
```

- [ ] **Step 2: Create `crates/ahandd/src/cli/browser_doctor.rs`**

```rust
use anyhow::Result;

use crate::browser_setup::{
    self, CheckReport, CheckStatus, FixHint, PlatformCommand,
};

/// Entry point for `ahandd browser-doctor`.
pub async fn run() -> Result<()> {
    println!("Browser Automation Diagnostics");
    println!("==============================");

    let reports = browser_setup::inspect_all().await;
    for report in &reports {
        print_check(report);
    }
    println!();

    let failures: Vec<&CheckReport> = reports
        .iter()
        .filter(|r| !matches!(r.status, CheckStatus::Ok { .. }))
        .collect();

    if failures.is_empty() {
        println!("Status: all checks passed.");
        return Ok(());
    }

    println!("Status: {} issue(s) found.", failures.len());
    println!();
    println!("Fix suggestions:");
    for failure in &failures {
        if let Some(hint) = &failure.fix_hint {
            print_fix_hint(failure.label, hint);
        }
    }

    std::process::exit(1);
}

fn print_check(report: &CheckReport) {
    let (marker, line) = match &report.status {
        CheckStatus::Ok { version, path, source } => {
            let suffix = match source {
                crate::browser_setup::CheckSource::Managed => String::new(),
                crate::browser_setup::CheckSource::System => " (system)".into(),
                crate::browser_setup::CheckSource::Preinstalled => " (preinstalled)".into(),
            };
            let version_str = if version.is_empty() {
                String::new()
            } else {
                format!("{version}  ")
            };
            (
                "[\u{2713}]",
                format!("{:<17} {version_str}({}){suffix}", format!("{}:", report.label), path.display()),
            )
        }
        CheckStatus::Missing => (
            "[\u{2717}]",
            format!("{:<17} not found", format!("{}:", report.label)),
        ),
        CheckStatus::Outdated { current, required, path } => (
            "[\u{2717}]",
            format!(
                "{:<17} {current} (need {required}) at {}",
                format!("{}:", report.label),
                path.display()
            ),
        ),
        CheckStatus::NoneDetected { tried } => (
            "[\u{2717}]",
            format!(
                "{:<17} none detected\n                     Tried: {}",
                format!("{}:", report.label),
                tried.join(", ")
            ),
        ),
    };
    println!("{marker} {line}");
}

fn print_fix_hint(label: &str, hint: &FixHint) {
    match hint {
        FixHint::RunStep { command } => {
            println!("  {label}  →  {command}");
        }
        FixHint::ManualCommand { platform_commands } => {
            println!("  {label}:");
            for PlatformCommand { platform, command } in platform_commands {
                println!("    {platform:<8}  {command}");
            }
        }
    }
}
```

- [ ] **Step 3: Create `crates/ahandd/src/cli/browser_init.rs`**

```rust
use anyhow::Result;

use crate::browser_setup::{self, Phase, ProgressEvent};

/// Entry point for `ahandd browser-init [--force] [--step <name>]`.
pub async fn run(force: bool, step: Option<String>) -> Result<()> {
    let progress = make_progress_printer();

    match step.as_deref() {
        Some(name) => {
            let report = browser_setup::run_step(name, force, progress).await?;
            println!();
            println!("Step `{name}` complete.");
            print_summary(&[report]);
        }
        None => {
            let reports = browser_setup::run_all(force, progress).await?;
            println!();
            println!("Setup complete.");
            print_summary(&reports);
        }
    }
    Ok(())
}

fn make_progress_printer() -> impl Fn(ProgressEvent) + Send + Sync + 'static {
    |event: ProgressEvent| match event.phase {
        Phase::Done => println!("  \u{2713} {}", event.message),
        Phase::Starting | Phase::Downloading | Phase::Extracting | Phase::Installing | Phase::Verifying => {
            println!("  {}", event.message);
        }
    }
}

fn print_summary(reports: &[browser_setup::CheckReport]) {
    use browser_setup::CheckStatus;
    for report in reports {
        match &report.status {
            CheckStatus::Ok { version, path, .. } => {
                let version_str = if version.is_empty() { String::new() } else { format!(" {version}") };
                println!("  {}:{} ({})", report.label, version_str, path.display());
            }
            CheckStatus::Missing => {
                println!("  {}: still missing", report.label);
            }
            CheckStatus::Outdated { current, required, .. } => {
                println!("  {}: {current} (need {required})", report.label);
            }
            CheckStatus::NoneDetected { tried } => {
                println!("  {}: none detected (tried: {})", report.label, tried.join(", "));
            }
        }
    }
}
```

- [ ] **Step 4: Update `crates/ahandd/src/main.rs`**

Add `mod cli;` near the top of the module list:

```rust
mod ahand_client;
mod approval;
mod browser;
mod browser_setup;
mod cli;
mod config;
```

Add `BrowserDoctor` variant to the `Cmd` enum:

```rust
#[derive(Subcommand)]
enum Cmd {
    /// Initialize browser automation dependencies
    BrowserInit {
        /// Force reinstall (clean existing installation first)
        #[arg(long)]
        force: bool,
        /// Run only a single step (node or playwright)
        #[arg(long)]
        step: Option<String>,
    },
    /// Diagnose browser automation setup and report missing components
    BrowserDoctor,
}
```

Route both through the CLI adapters:

```rust
    if let Some(cmd) = &args.command {
        match cmd {
            Cmd::BrowserInit { force, step } => {
                return cli::browser_init::run(*force, step.clone()).await;
            }
            Cmd::BrowserDoctor => {
                return cli::browser_doctor::run().await;
            }
        }
    }
```

- [ ] **Step 5: Verify compilation**

Run: `cargo check -p ahandd`
Expected: No errors

- [ ] **Step 6: Manual smoke test — browser-doctor**

Run: `cargo run -p ahandd -- browser-doctor`
Expected: Prints the three check lines with appropriate markers. Exit code is 0 if everything is set up, 1 otherwise.

- [ ] **Step 7: Manual smoke test — browser-init step rejection**

Run: `cargo run -p ahandd -- browser-init --step unknown`
Expected: Error message: `unknown step \`unknown\`. Valid steps: node, playwright` and exit code ≠ 0.

- [ ] **Step 8: Manual smoke test — step dependency check**

Temporarily rename `~/.ahand/node/` to `~/.ahand/node.bak/` (if it exists), then run:

```bash
cargo run -p ahandd -- browser-init --step playwright
```

Expected: Error message mentioning that node must be installed first. Then restore the directory:

```bash
mv ~/.ahand/node.bak ~/.ahand/node  # if you renamed it
```

- [ ] **Step 9: Commit**

```bash
git add crates/ahandd/src/cli/ crates/ahandd/src/main.rs
git commit -m "feat(ahandd): add browser-doctor command and CLI adapters"
```

---

### Task 7: Integration test for browser-doctor exit code

**Goal:** Add a lightweight integration test that verifies `ahandd browser-doctor` returns exit code 0/1 correctly based on the environment.

**Files:**
- Create: `crates/ahandd/tests/browser_doctor.rs`

**Acceptance Criteria:**
- [ ] Test spawns `cargo run -p ahandd -- browser-doctor` as a subprocess
- [ ] Test passes regardless of whether the current environment has Node/playwright installed (it only checks the exit code is 0 or 1, never crashes)
- [ ] Test fails if the binary panics or returns a non-0/1 exit code

**Verify:** `cargo test -p ahandd --test browser_doctor` → passes

**Steps:**

- [ ] **Step 1: Create `crates/ahandd/tests/browser_doctor.rs`**

```rust
//! Smoke test for `ahandd browser-doctor`.
//!
//! This test doesn't assert specific output — it only verifies the command
//! exits cleanly with either 0 (all checks pass) or 1 (some checks fail).
//! Any other outcome (panic, non-zero/non-one exit, hang) is a bug.

use std::process::Command;
use std::time::Duration;

#[test]
fn browser_doctor_exits_with_zero_or_one() {
    // Build the binary first so the test doesn't time out on cold compilation.
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "ahandd", "--bin", "ahandd"])
        .status()
        .expect("cargo build failed to start");
    assert!(status.success(), "cargo build failed");

    // Locate the built binary.
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .unwrap_or_else(|_| "target".into());
    let bin = format!("{target_dir}/debug/ahandd");

    let mut child = Command::new(&bin)
        .arg("browser-doctor")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn ahandd browser-doctor");

    // Wait up to 10 seconds — doctor shouldn't take anywhere near this long.
    let start = std::time::Instant::now();
    loop {
        match child.try_wait().unwrap() {
            Some(status) => {
                let code = status.code().unwrap_or(-1);
                assert!(
                    code == 0 || code == 1,
                    "browser-doctor returned unexpected exit code: {code}"
                );
                return;
            }
            None => {
                if start.elapsed() > Duration::from_secs(10) {
                    let _ = child.kill();
                    panic!("browser-doctor took more than 10s to finish");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p ahandd --test browser_doctor`
Expected: Passes (builds the binary once, then runs the smoke test)

- [ ] **Step 3: Commit**

```bash
git add crates/ahandd/tests/browser_doctor.rs
git commit -m "test(ahandd): smoke test browser-doctor exit code"
```
