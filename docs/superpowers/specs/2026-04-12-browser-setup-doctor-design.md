# Browser Setup Doctor & Modular Init Design Spec

**Date:** 2026-04-12
**Status:** Draft

## Overview

Refactor the daemon's browser setup flow to:

1. Add a `browser-doctor` subcommand that diagnoses what's installed vs missing and prints fix hints.
2. Extend `browser-init` to support running individual steps (`--step node`, `--step playwright`).
3. Expand browser detection to include Microsoft Edge (preinstalled on Windows) and add full Windows path support.
4. Extract the setup logic into a library-friendly module so a future Tauri app can reuse it directly from Rust instead of parsing CLI output.

## Motivation

- **Discoverability:** Users currently hit cryptic runtime errors when dependencies are missing. A `browser-doctor` command gives them a single place to see what's wrong and how to fix it.
- **Granular recovery:** When only one piece breaks (e.g., npm reinstall corrupts `playwright-cli`), forcing a full reinstall wastes bandwidth. Single-step mode targets just the broken piece.
- **Windows support:** Browser detection today has no Windows branch at all. Adding Edge as a fallback gives Windows users zero-install browser automation (Edge is preinstalled on Windows 10+).
- **Tauri reuse:** The team is likely to build a Tauri desktop app that needs the same setup flow. Designing the core as a pure library (structured return types, progress callbacks) makes that reuse trivial — no CLI output parsing, no duplicate logic.

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Core/UI separation | Core library returns structured data + progress callback; CLI layer formats | Enables Tauri and CLI to share 100% of setup logic |
| Browser list | Chrome, Chromium, Edge | Covers ~99% of users. Edge is preinstalled on Windows; Brave adds maintenance burden without real coverage |
| `--step browser` | Not provided | System browser install involves package managers/permissions; doctor prints instructions, user runs them |
| Step dependency handling | Explicit error, not auto-install | Single-step mode users know what they want; implicit dependencies make behavior unpredictable |
| Progress API | `impl Fn(ProgressEvent)` callback | Simplest universal interface; Tauri handles IPC/frontend concerns on its own side |

## Part 1: Architecture — Library-First Refactor

### Module Structure

```
crates/ahandd/src/
├── browser_setup/               (renamed from browser_init, now a directory)
│   ├── mod.rs                   Public API: inspect_all, run_all, run_step + re-exports
│   ├── types.rs                 CheckStatus, CheckReport, ProgressEvent, FixHint, DetectedBrowser
│   ├── node.rs                  Node.js check + install (from old browser_init.rs)
│   ├── playwright.rs            playwright-cli check + install (from old browser_init.rs)
│   └── browser_detect.rs        System browser detection (Chrome/Chromium/Edge)
├── browser.rs                   Uses browser_setup::browser_detect::detect() — no duplicate logic
└── cli/
    ├── mod.rs                   pub mod browser_doctor; pub mod browser_init;
    ├── browser_doctor.rs        Formats CheckReport → terminal output, exit code
    └── browser_init.rs          Calls run_all/run_step, shows progress in terminal
```

### Core Library Principles

- **No `println!` in `browser_setup/`.** All output flows through the progress callback or the final `CheckReport` return value.
- **No `clap` imports in `browser_setup/`.** CLI parsing lives in `cli/` and `main.rs`.
- **Structured returns.** `inspect_all()` returns `Vec<CheckReport>`; callers decide how to render.
- **`Serialize` on public types** so Tauri can `emit` them to the frontend without transformation.

### Core Types

```rust
#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    pub name: &'static str,      // "node" / "playwright" / "browser"
    pub label: &'static str,     // "Node.js" / "playwright-cli" / "System Browser"
    pub status: CheckStatus,
    pub fix_hint: Option<FixHint>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckStatus {
    Ok {
        version: String,
        path: PathBuf,
        source: CheckSource,
    },
    Missing,
    Outdated {
        current: String,
        required: String,
        path: PathBuf,
    },
    NoneDetected {
        tried: Vec<String>,       // e.g. ["Google Chrome", "Chromium", "Microsoft Edge"]
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckSource {
    Managed,       // installed by ahandd (~/.ahand/node/...)
    System,        // system-wide install (Chrome, system Node)
    Preinstalled,  // OS shipped (Edge on Windows)
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FixHint {
    RunStep { command: String },                               // "ahandd browser-init --step node"
    ManualCommand { platform_commands: Vec<PlatformCommand> }, // user runs manually
}

#[derive(Debug, Clone, Serialize)]
pub struct PlatformCommand {
    pub platform: &'static str,  // "macOS" / "Linux" / "Windows"
    pub command: String,         // "brew install --cask google-chrome"
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgressEvent {
    pub step: &'static str,      // "node" / "playwright"
    pub phase: Phase,
    pub message: String,
    pub percent: Option<u8>,     // 0-100, only for measurable operations (downloads)
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

#[derive(Debug, Clone, Serialize)]
pub struct DetectedBrowser {
    pub name: String,            // "Google Chrome" / "Microsoft Edge" / ...
    pub path: PathBuf,
    pub kind: BrowserKind,
    pub source: CheckSource,     // System / Preinstalled
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserKind {
    Chrome,
    Chromium,
    Edge,
}
```

### Public API

```rust
// Diagnostic — read-only
pub async fn inspect_all() -> Vec<CheckReport>;
pub async fn inspect(name: &str) -> Option<CheckReport>;

// Install — mutations, reports progress via callback
pub async fn run_all(
    force: bool,
    progress: impl Fn(ProgressEvent) + Send + 'static,
) -> Result<Vec<CheckReport>>;

pub async fn run_step(
    name: &str,
    force: bool,
    progress: impl Fn(ProgressEvent) + Send + 'static,
) -> Result<CheckReport>;

// Browser detection — exposed separately since BrowserManager uses it too
pub fn detect_browser(config_override: Option<&str>) -> Option<DetectedBrowser>;
pub fn detect_all_browsers() -> Vec<DetectedBrowser>;
```

## Part 2: `browser-doctor` Command

### Signature

```
ahandd browser-doctor
```

No arguments. Read-only — never modifies files.

### Execution Flow

1. Call `browser_setup::inspect_all()`.
2. For each `CheckReport`, format a status line.
3. Print a summary.
4. If all checks pass → exit 0. If any fail → print fix hints and exit 1.

### Output Examples

**All OK:**

```
Browser Automation Diagnostics
==============================
[✓] Node.js:         v24.13.0  (~/.ahand/node/bin/node)
[✓] playwright-cli:  0.1.1     (~/.ahand/node/bin/playwright-cli)
[✓] System Browser:  Google Chrome
                     /Applications/Google Chrome.app/Contents/MacOS/Google Chrome

Status: all checks passed.
```

**One or more issues:**

```
Browser Automation Diagnostics
==============================
[✓] Node.js:         v24.13.0  (~/.ahand/node/bin/node)
[✗] playwright-cli:  not found
[✓] System Browser:  Microsoft Edge (preinstalled)
                     C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe

Status: 1 issue found.

Fix suggestions:
  playwright-cli  →  ahandd browser-init --step playwright
```

**No browser detected:**

```
[✗] System Browser:  none detected
                     Tried: Chrome, Chromium, Edge

                     Install one of:
                       macOS:    brew install --cask google-chrome
                       Linux:    sudo apt install chromium-browser
                                 (or microsoft-edge-stable)
                       Windows:  Edge should be preinstalled — please report
```

### Exit Codes

- `0` — all checks passed
- `1` — at least one check failed (useful for CI scripts)

## Part 3: Browser Detection Expansion

### Current State

`BrowserManager::resolve_executable_path()` in `src/browser.rs:497` only handles:
- macOS Chrome variants (stable/dev/canary/Chromium)
- Linux Chrome variants (`google-chrome`, `google-chrome-stable`)

No Edge detection, no Windows branch at all.

### New Detection Order

Priority: respected `BrowserConfig::executable_path` override first, then platform-specific list.

**macOS:**

| # | Browser | Path |
|---|---------|------|
| 1 | Google Chrome | `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome` |
| 2 | Google Chrome Dev | `/Applications/Google Chrome Dev.app/Contents/MacOS/Google Chrome Dev` |
| 3 | Google Chrome Canary | `/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary` |
| 4 | Chromium | `/Applications/Chromium.app/Contents/MacOS/Chromium` |
| 5 | Microsoft Edge | `/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge` |

**Linux:**

| # | Browser | Path |
|---|---------|------|
| 1 | google-chrome-stable | `/usr/bin/google-chrome-stable` |
| 2 | google-chrome | `/usr/bin/google-chrome` |
| 3 | chromium | `/usr/bin/chromium` |
| 4 | chromium-browser | `/usr/bin/chromium-browser` |
| 5 | microsoft-edge-stable | `/usr/bin/microsoft-edge-stable` |
| 6 | microsoft-edge | `/usr/bin/microsoft-edge` |

**Windows (new):**

| # | Browser | Path |
|---|---------|------|
| 1 | Google Chrome | `C:\Program Files\Google\Chrome\Application\chrome.exe` |
| 2 | Google Chrome (x86) | `C:\Program Files (x86)\Google\Chrome\Application\chrome.exe` |
| 3 | Microsoft Edge (x86) | `C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe` |
| 4 | Microsoft Edge (x64) | `C:\Program Files\Microsoft\Edge\Application\msedge.exe` |

Chrome takes priority (most compatible with Playwright), with Edge as the fallback. Since Edge is preinstalled on Windows 10+, Windows users get zero-install browser automation out of the box.

### Integration with `BrowserManager`

`src/browser.rs`:

```rust
fn resolve_executable_path(&self) -> Option<String> {
    browser_setup::detect_browser(self.config.executable_path.as_deref())
        .map(|b| b.path.to_string_lossy().into_owned())
}
```

One line. The old inline match block is deleted.

## Part 4: `browser-init --step` Single-Step Support

### Signature

```
ahandd browser-init [--force] [--step <name>]
```

- `--step <name>` optional. Allowed values: `node`, `playwright`.
- Without `--step`: runs all steps (current behavior preserved).
- `--force` and `--step` can combine: `--force --step playwright` reinstalls only playwright-cli.

### Deliberate Omission: `--step browser`

System browser installation requires package managers (`brew`, `apt`, `choco`) or manual downloads with permission/sudo implications. `browser-init` does not do this. Instead, `browser-doctor` prints manual install instructions per platform and the user runs them themselves.

### Execution Logic

```rust
pub async fn run_step(
    name: &str,
    force: bool,
    progress: impl Fn(ProgressEvent) + Send + 'static,
) -> Result<CheckReport> {
    match name {
        "node" => node::ensure(force, progress).await,
        "playwright" => {
            // Check dependency
            let node_report = node::inspect().await;
            if !matches!(node_report.status, CheckStatus::Ok { .. }) {
                bail!(
                    "playwright step requires node to be installed first. \
                     Run `ahandd browser-init --step node` first, or \
                     `ahandd browser-init` for all steps."
                );
            }
            playwright::ensure(force, progress).await
        }
        other => bail!("unknown step `{other}`. Valid steps: node, playwright"),
    }
}
```

### Dependency Handling

- `node` → no dependencies
- `playwright` → depends on `node`. On missing dependency, **fail with a clear error** rather than auto-installing. Single-step users expect predictable, minimal behavior.

### `--force --step` Combinations

| Command | Effect |
|---------|--------|
| `ahandd browser-init` | Run all steps (Node + playwright), install only missing |
| `ahandd browser-init --force` | Clean everything, reinstall all |
| `ahandd browser-init --step node` | Install Node only if missing |
| `ahandd browser-init --force --step node` | Remove `~/.ahand/node/`, reinstall Node, leave playwright alone |
| `ahandd browser-init --step playwright` | Install playwright-cli only if missing (error if Node missing) |
| `ahandd browser-init --force --step playwright` | Uninstall + reinstall playwright-cli, do not touch Node |

## Part 5: Progress Reporting

### API Choice: Callback

The core library accepts `impl Fn(ProgressEvent) + Send + 'static` as the progress reporter. This is the simplest universal interface:

- **CLI**: callback writes to stdout
- **Tauri**: callback calls `window.emit("browser-setup-progress", event)` — Tauri handles frontend IPC internally
- **Tests**: callback pushes events into a `Vec` for assertions

The core library stays oblivious to Tauri or CLI concerns. `ProgressEvent` derives `Serialize` so Tauri can emit it without transformation.

### Progress Granularity

Not every operation has measurable progress. `percent: Option<u8>` is `Some` only when measurable (e.g., a download with known total size). npm install and extraction steps report `percent: None` and rely on the `Phase` + `message` to convey state.

### CLI Usage

```rust
// In cli/browser_init.rs:
let progress = |event: ProgressEvent| {
    match event.phase {
        Phase::Downloading if event.percent.is_some() => {
            let pct = event.percent.unwrap();
            print!("\r  {} ({pct}%)", event.message);
            io::stdout().flush().ok();
        }
        Phase::Done => println!("  ✓ {}", event.message),
        _ => println!("  {}", event.message),
    }
};

let reports = browser_setup::run_all(args.force, progress).await?;
```

### Tauri Usage (Future)

```rust
// In future Tauri app:
#[tauri::command]
async fn browser_setup_run_all(
    window: tauri::Window,
    force: bool,
) -> Result<Vec<CheckReport>, String> {
    let progress = move |event: ProgressEvent| {
        let _ = window.emit("browser-setup-progress", &event);
    };
    ahandd::browser_setup::run_all(force, progress)
        .await
        .map_err(|e| e.to_string())
}
```

Tauri's frontend subscribes to `browser-setup-progress` and renders a live UI. The aHand core library has no knowledge of this.

## Files to Create / Modify

### Created

- `crates/ahandd/src/browser_setup/mod.rs` — public API surface
- `crates/ahandd/src/browser_setup/types.rs` — `CheckStatus`, `CheckReport`, `ProgressEvent`, etc.
- `crates/ahandd/src/browser_setup/node.rs` — Node.js check + install (moved from `browser_init.rs`)
- `crates/ahandd/src/browser_setup/playwright.rs` — playwright-cli check + install (moved from `browser_init.rs`)
- `crates/ahandd/src/browser_setup/browser_detect.rs` — system browser detection (extracted from `browser.rs`)
- `crates/ahandd/src/cli/mod.rs` — CLI module index
- `crates/ahandd/src/cli/browser_doctor.rs` — terminal formatter for `browser-doctor`
- `crates/ahandd/src/cli/browser_init.rs` — terminal formatter for `browser-init`

### Modified

- `crates/ahandd/src/main.rs` — register `BrowserDoctor` subcommand, wire `--step` flag on `BrowserInit`, route to `cli::browser_doctor::run()` / `cli::browser_init::run()`
- `crates/ahandd/src/browser.rs` — replace `resolve_executable_path()` inline logic with `browser_setup::detect_browser()` call
- `crates/ahandd/src/lib.rs` (or `main.rs` module tree) — add `pub mod browser_setup;` and `mod cli;`

### Deleted

- `crates/ahandd/src/browser_init.rs` — replaced by `browser_setup/` directory

## Testing Strategy

### Unit Tests (in `browser_setup/`)

- **`browser_detect.rs`**: mock filesystem, verify correct priority order per platform; verify override path wins
- **`node.rs`**: mock `node -v` command; verify version parsing; verify `Outdated` status when major < 20
- **`playwright.rs`**: mock `playwright-cli --version` command; verify version comparison

### Integration Tests

Integration tests for `browser-doctor` and `browser-init` are hard because they touch the real filesystem, network, and subprocess execution. Keep integration coverage light:
- Smoke test: `ahandd browser-doctor` exits cleanly on a fresh test env (with all three checks missing) and exits with code 1
- Smoke test: `ahandd browser-init --step node` in isolation (behind a feature flag to avoid slow CI)

Heavy setup testing is manual — the spec reviewer runs the commands on macOS/Linux/Windows and confirms correct behavior.

## Out of Scope

- **Auto-installing system browsers** — deliberately omitted (see Part 4)
- **Caching download tarballs across installs** — potential future optimization, not needed now
- **Progress reporting for npm install** — npm's stdout is not parseable for reliable progress; we report Phase transitions only
- **Brave / Vivaldi / Arc detection** — small user base, config override handles the long tail
- **Extracting `browser_setup` into a standalone crate** — can do later without API changes since the module is already self-contained

## Why Not Just Keep the Flat `browser_init.rs`?

The current file is 343 lines and mixes concerns (download, extract, npm install, CLI output). Adding doctor + step support + Windows paths + Edge detection + library-friendly API would push it past 700 lines of tangled code. Splitting it now (while touching the file anyway) is cheaper than a dedicated cleanup pass later.
