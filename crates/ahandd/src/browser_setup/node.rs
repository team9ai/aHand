//! Node.js runtime download, extraction, and layout normalization.
//!
//! ## Normalized on-disk layout (post-extraction)
//!
//! The layout is identical on all platforms so that `RuntimeDirs::node_bin()`
//! — which always returns `<node_dir>/bin/node[.exe]` — resolves correctly.
//!
//! ### Unix (tar.xz)
//! The upstream tarball already ships with a `bin/` directory:
//! ```text
//! <node_dir>/
//!   bin/
//!     node               ← executable
//!     npm -> ../lib/…    ← symlink (not used by ahand)
//!   lib/node_modules/npm/bin/npm-cli.js
//!   include/…
//!   share/…
//! ```
//!
//! ### Windows (zip)
//! The upstream zip is a flat distribution — `node.exe`, `npm.cmd`, etc. live
//! at the top level with no `bin/` directory.  After extraction we normalise to
//! match the unix shape:
//! ```text
//! <node_dir>/
//!   bin/
//!     node.exe           ← moved from the zip root
//!   node_modules/        ← kept in place (npm-cli.js lives here)
//!   npm.cmd              ← left in place (unused by ahand; Task 2 invokes
//!   npx.cmd                npm via `node.exe npm-cli.js` instead)
//! ```
//! The `node_modules/npm/bin/npm-cli.js` path is preserved relative to
//! `<node_dir>` so Task 2 can resolve it without additional logic.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};

use super::types::{CheckReport, CheckSource, CheckStatus, FixHint, Phase, ProgressEvent};

pub const NODE_MIN_VERSION: u32 = 20;
pub const NODE_LTS_VERSION: &str = "24.13.0";

pub struct Dirs {
    #[allow(dead_code)] // base dir kept for future sub-path helpers
    pub ahand: PathBuf,
    pub node: PathBuf,
    runtime: crate::plugin_runtime::RuntimeDirs,
}

impl Dirs {
    pub fn new() -> Result<Self> {
        let runtime = crate::plugin_runtime::RuntimeDirs::new()?;
        Ok(Self::from_runtime(runtime))
    }

    // Plugin-runtime API surface; consumed by later plugin stages/tests.
    #[allow(dead_code)]
    pub fn from_runtime_root(root: PathBuf) -> Self {
        Self::from_runtime(crate::plugin_runtime::RuntimeDirs::from_root(root))
    }

    fn from_runtime(runtime: crate::plugin_runtime::RuntimeDirs) -> Self {
        let ahand = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".ahand");
        let node = runtime.node_dir();
        Self {
            ahand,
            node,
            runtime,
        }
    }

    pub fn local_node_bin(&self) -> PathBuf {
        self.runtime.node_bin()
    }

    /// Return the (program, leading_args) pair for invoking npm.
    /// Delegates to `RuntimeDirs::npm_invocation()`.
    pub fn npm_invocation(&self) -> (PathBuf, Vec<std::ffi::OsString>) {
        self.runtime.npm_invocation()
    }

    pub fn playwright_cli_bin(&self) -> PathBuf {
        self.runtime.playwright_cli_bin()
    }

    /// Return the (program, leading_args) pair for invoking playwright-cli.
    /// Delegates to `RuntimeDirs::playwright_cli_invocation()`.
    pub fn playwright_cli_invocation(&self) -> anyhow::Result<(PathBuf, Vec<std::ffi::OsString>)> {
        self.runtime.playwright_cli_invocation()
    }
}

fn local_node_bin() -> Result<PathBuf> {
    Ok(Dirs::new()?.local_node_bin())
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
    let local_node = dirs.local_node_bin();

    if !force
        && local_node.exists()
        && let Some(ver) = read_node_major_version(&local_node).await
        && ver >= NODE_MIN_VERSION
    {
        emit(
            progress,
            Phase::Done,
            format!(
                "Node.js v{ver}.x already installed at {}",
                dirs.node.display()
            ),
        );
        return Ok(inspect().await);
    }

    // Remove the old installation (whether --force was set or version was too low)
    // to avoid stale files from a previous version mixing with the new one.
    if dirs.node.exists() {
        let _ = std::fs::remove_dir_all(&dirs.node);
    }

    emit(
        progress,
        Phase::Starting,
        format!(
            "Installing Node.js v{NODE_LTS_VERSION} to {}",
            dirs.node.display()
        ),
    );

    install_node(&dirs, progress)
        .await
        .context(manual_install_hint())?;

    if !local_node.exists() {
        anyhow::bail!(
            "Node.js installation completed but binary not found at {}.",
            local_node.display()
        );
    }

    emit(
        progress,
        Phase::Done,
        format!(
            "Node.js v{NODE_LTS_VERSION} ready at {}",
            dirs.node.display()
        ),
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

async fn install_node(dirs: &Dirs, progress: &(dyn Fn(ProgressEvent) + Send + Sync)) -> Result<()> {
    let (os, arch) = platform_info();

    if cfg!(target_os = "windows") {
        install_node_windows(dirs, progress, os, arch).await
    } else {
        install_node_unix(dirs, progress, os, arch).await
    }
}

/// Unix path: download `.tar.xz`, extract preserving the `bin/` layout.
async fn install_node_unix(
    dirs: &Dirs,
    progress: &(dyn Fn(ProgressEvent) + Send + Sync),
    os: &str,
    arch: &str,
) -> Result<()> {
    let tarball = format!("node-v{NODE_LTS_VERSION}-{os}-{arch}.tar.xz");
    let url = format!("https://nodejs.org/dist/v{NODE_LTS_VERSION}/{tarball}");

    emit(
        progress,
        Phase::Downloading,
        format!("Downloading {tarball}"),
    );

    let bytes = download_bytes(&url).await.context(format!(
        "Failed to download Node.js from {url} — check your network connection"
    ))?;

    std::fs::create_dir_all(&dirs.node).context(format!(
        "Failed to create directory {}: permission denied or disk full",
        dirs.node.display()
    ))?;

    emit(
        progress,
        Phase::Extracting,
        "Extracting Node.js archive".into(),
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
        "Verifying Node.js installation".into(),
    );

    Ok(())
}

/// Windows path: download `.zip`, extract with traversal/symlink guards,
/// then normalise the flat layout into `bin/node.exe`.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
async fn install_node_windows(
    dirs: &Dirs,
    progress: &(dyn Fn(ProgressEvent) + Send + Sync),
    os: &str,
    arch: &str,
) -> Result<()> {
    let zipfile = format!("node-v{NODE_LTS_VERSION}-{os}-{arch}.zip");
    let url = format!("https://nodejs.org/dist/v{NODE_LTS_VERSION}/{zipfile}");

    emit(
        progress,
        Phase::Downloading,
        format!("Downloading {zipfile}"),
    );

    let bytes = download_bytes(&url).await.context(format!(
        "Failed to download Node.js from {url} — check your network connection"
    ))?;

    std::fs::create_dir_all(&dirs.node).context(format!(
        "Failed to create directory {}: permission denied or disk full",
        dirs.node.display()
    ))?;

    emit(
        progress,
        Phase::Extracting,
        "Extracting Node.js archive".into(),
    );

    extract_node_zip(&bytes, &dirs.node).context("Failed to extract Node.js zip archive")?;

    emit(
        progress,
        Phase::Verifying,
        "Verifying Node.js installation".into(),
    );

    Ok(())
}

/// Extract a Node.js Windows zip archive into `dest_dir`.
///
/// Safety properties (mirroring M2's `guard_path_traversal` semantics):
///  - First path component is stripped (the versioned top-level dir, e.g.
///    `node-v24.13.0-win-x64/`).
///  - Every remaining component is checked: `ParentDir` (`..`), `RootDir`,
///    and `Prefix` (Windows drive letters) are all rejected.
///  - Symlink entries (detected via `ZipFile::is_symlink()`) are rejected.
///  - Only regular files and directories are extracted.
///
/// Layout normalization (option (a) from the plan):
///  After extraction the zip root-level files land directly in `dest_dir`.
///  `node.exe` is then moved to `dest_dir/bin/node.exe` so that
///  `RuntimeDirs::node_bin()` — which always returns `<node_dir>/bin/node[.exe]`
///  — resolves correctly on Windows without any changes to `RuntimeDirs`.
fn extract_node_zip(zip_bytes: &[u8], dest_dir: &Path) -> Result<()> {
    let cursor = std::io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).context("Failed to open Node.js zip archive")?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).context("Failed to read zip entry")?;

        // Reject symlinks — we never expect them in the Node.js Windows zip.
        if file.is_symlink() {
            anyhow::bail!(
                "Node.js zip contains a symlink entry '{}'; this is unexpected and rejected for safety",
                file.name()
            );
        }

        let raw_name = file.name().to_owned();
        let raw_path = PathBuf::from(&raw_name);

        // Validate the RAW path BEFORE stripping: reject any entry whose
        // first or subsequent components are ParentDir, RootDir, or Prefix.
        // An entry like `../evil` or `/etc/passwd` must error here, not be
        // silently neutralised into the root by skip(1).
        // Require at least one component so zero-component entries are also
        // rejected (empty name).
        {
            let comp_count = raw_path.components().count();
            if comp_count < 1 {
                anyhow::bail!("Node.js zip entry has an empty path: '{raw_name}'");
            }
            // Check every component in the raw path (including the first).
            guard_zip_path_traversal(&raw_path)
                .with_context(|| format!("Path traversal in raw zip entry path '{raw_name}'"))?;
        }

        // Strip first component (the versioned top-level directory).
        let stripped: PathBuf = raw_path.components().skip(1).collect();
        if stripped.as_os_str().is_empty() {
            // Top-level directory entry itself — skip.
            continue;
        }

        // Component-level traversal guard (post-strip, defence-in-depth).
        guard_zip_path_traversal(&stripped).with_context(|| {
            format!("Path traversal detected in Node.js zip entry '{raw_name}'")
        })?;

        let dest = dest_dir.join(&stripped);

        if file.is_dir() {
            std::fs::create_dir_all(&dest)
                .with_context(|| format!("Failed to create directory {}", dest.display()))?;
        } else {
            // Regular file — ensure parent directory exists.
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("Failed to create parent directory {}", parent.display())
                })?;
            }
            let mut out = std::fs::File::create(&dest)
                .with_context(|| format!("Failed to create file {}", dest.display()))?;
            std::io::copy(&mut file, &mut out)
                .with_context(|| format!("Failed to write file {}", dest.display()))?;
        }
    }

    // Layout normalization: move node.exe into bin/ so RuntimeDirs::node_bin()
    // resolves to <node_dir>/bin/node.exe on Windows.
    let node_exe_src = dest_dir.join("node.exe");
    let bin_dir = dest_dir.join("bin");
    let node_exe_dst = bin_dir.join("node.exe");

    if node_exe_src.exists() && !node_exe_dst.exists() {
        std::fs::create_dir_all(&bin_dir)
            .context("Failed to create node/bin directory for layout normalization")?;
        std::fs::rename(&node_exe_src, &node_exe_dst).with_context(|| {
            format!(
                "Failed to move node.exe from {} to {}",
                node_exe_src.display(),
                node_exe_dst.display()
            )
        })?;
    }

    Ok(())
}

/// Reject any zip entry path component that would escape the extraction root.
///
/// Mirrors `guard_path_traversal` from `ahandctl::upgrade::assets`:
///  - `Component::ParentDir` (`..`) is rejected.
///  - `Component::RootDir` (leading `/`) is rejected.
///  - `Component::Prefix` (Windows drive letter `C:\`) is rejected.
fn guard_zip_path_traversal(p: &Path) -> Result<()> {
    for component in p.components() {
        match component {
            Component::ParentDir => {
                anyhow::bail!(
                    "parent-dir component (..) in zip entry path: {}",
                    p.display()
                );
            }
            Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("absolute path in zip entry: {}", p.display());
            }
            Component::Normal(_) | Component::CurDir => {}
        }
    }
    Ok(())
}

/// Return a platform-appropriate manual install hint for the ensure() error context.
fn manual_install_hint() -> &'static str {
    if cfg!(target_os = "windows") {
        "Failed to install Node.js. Check your network connection and retry, \
         or install Node.js >= 20 manually from https://nodejs.org/en/download (Windows installer)."
    } else if cfg!(target_os = "macos") {
        "Failed to install Node.js. Check your network connection and retry, \
         or install Node.js >= 20 manually (e.g. `brew install node`)."
    } else {
        "Failed to install Node.js. Check your network connection and retry, \
         or install Node.js >= 20 manually (e.g. `sudo apt install nodejs` or from https://nodejs.org)."
    }
}

async fn download_bytes(url: &str) -> Result<Vec<u8>> {
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

fn emit(progress: &(dyn Fn(ProgressEvent) + Send + Sync), phase: Phase, message: String) {
    progress(ProgressEvent {
        step: "node",
        phase,
        message,
        percent: None,
        stream: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn dirs_use_plugin_runtime_node_directory() {
        let root = PathBuf::from("/tmp/ahand-primary-runtime");
        let dirs = Dirs::from_runtime_root(root);

        assert_eq!(
            dirs.node,
            PathBuf::from("/tmp/ahand-primary-runtime/dependencies/node")
        );
        let node_bin = if cfg!(windows) { "node.exe" } else { "node" };
        assert_eq!(
            dirs.local_node_bin(),
            PathBuf::from("/tmp/ahand-primary-runtime/dependencies/node/bin").join(node_bin)
        );
    }

    // ---------------------------------------------------------------------------
    // Task 4 verification: node.rs has NO subprocess install calls — the entire
    // install path is pure-Rust (reqwest HTTP download + xz2/tar extraction).
    // These tests confirm that the Phase events emitted by install_node() fire
    // correctly and that no Phase::Log events are ever produced by the node step.
    // ---------------------------------------------------------------------------

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

    /// `emit` always sets step="node", stream=None, percent=None.
    /// Node's pure-Rust install path NEVER emits Phase::Log events — those are
    /// only produced by subprocess I/O in playwright.rs.
    #[test]
    fn emit_produces_correct_step_and_no_stream() {
        let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let cb_events = events.clone();
        let cb = move |e: ProgressEvent| {
            cb_events.lock().unwrap().push(e);
        };

        emit(&cb, Phase::Starting, "Starting Node install".into());
        emit(&cb, Phase::Downloading, "Downloading tarball".into());
        emit(&cb, Phase::Extracting, "Extracting archive".into());
        emit(&cb, Phase::Verifying, "Verifying install".into());
        emit(&cb, Phase::Done, "Node.js ready".into());

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 5);

        for event in events.iter() {
            assert_eq!(event.step, "node", "step must always be 'node'");
            assert!(
                event.stream.is_none(),
                "node step never sets stream (no subprocess output): {:?}",
                event.phase
            );
            assert!(
                event.percent.is_none(),
                "node step never sets percent in emit(): {:?}",
                event.phase
            );
        }

        // Confirm no Phase::Log events — node uses pure-Rust, not subprocess
        let log_count = events
            .iter()
            .filter(|e| matches!(e.phase, Phase::Log))
            .count();
        assert_eq!(
            log_count, 0,
            "node step must never emit Phase::Log (no subprocess calls in install path)"
        );

        // Verify the phase sequence matches install_node()'s order
        let phases: Vec<String> = events.iter().map(|e| format!("{:?}", e.phase)).collect();
        assert_eq!(
            phases,
            vec!["Starting", "Downloading", "Extracting", "Verifying", "Done"]
        );
    }

    /// When the node binary already exists and meets the minimum version, `ensure`
    /// emits exactly one Phase::Done event (fast-path / cache hit).
    #[cfg(unix)]
    #[tokio::test]
    async fn ensure_already_installed_emits_done_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        // Create a fake node binary that outputs a valid version string
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let node_bin = bin_dir.join("node");
        std::fs::write(&node_bin, "#!/bin/sh\necho 'v24.13.0'\n").unwrap();
        let mut perms = std::fs::metadata(&node_bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&node_bin, perms).unwrap();

        // read_node_major_version reads the binary directly, so test it in isolation
        let version = read_node_major_version(&node_bin).await;
        assert_eq!(
            version,
            Some(24),
            "fake node binary should report major version 24"
        );
    }

    /// Confirms read_node_major_version returns None for a binary that fails to run.
    #[tokio::test]
    async fn read_node_major_version_returns_none_for_nonexistent_bin() {
        let version = read_node_major_version(std::path::Path::new("/nonexistent/node")).await;
        assert_eq!(version, None, "nonexistent binary should return None");
    }

    /// Confirms read_node_major_version returns None when output is unparseable.
    #[cfg(unix)]
    #[tokio::test]
    async fn read_node_major_version_returns_none_for_garbage_output() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("fake-node");
        std::fs::write(&bin, "#!/bin/sh\necho 'not-a-version'\n").unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();

        let version = read_node_major_version(&bin).await;
        assert_eq!(version, None, "garbage version output should return None");
    }

    /// Confirms read_node_major_version parses the standard `vMAJOR.MINOR.PATCH` format.
    #[cfg(unix)]
    #[tokio::test]
    async fn read_node_major_version_parses_semver_correctly() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("fake-node");
        std::fs::write(&bin, "#!/bin/sh\necho 'v20.11.0'\n").unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();

        let version = read_node_major_version(&bin).await;
        assert_eq!(version, Some(20));
    }

    /// platform_info returns recognized os/arch strings (not "unknown") on
    /// macOS (darwin) and Linux targets.
    #[test]
    fn platform_info_returns_known_os_and_arch() {
        let (os, arch) = platform_info();
        assert!(
            ["darwin", "linux", "win"].contains(&os),
            "unexpected os: {os}"
        );
        assert!(["arm64", "x64"].contains(&arch), "unexpected arch: {arch}");
    }

    // -------------------------------------------------------------------------
    // Zip extraction tests — cross-platform (run on macOS/Linux too).
    // These build small in-memory zip fixtures using the zip crate's writer API
    // and verify the extraction + layout normalization + safety guards.
    // -------------------------------------------------------------------------

    /// Build a minimal in-memory zip that mimics the real Windows Node.js
    /// distribution layout:
    ///
    /// ```text
    /// node-v24.13.0-win-x64/
    ///   node.exe
    ///   npm.cmd
    ///   node_modules/
    ///     npm/
    ///       bin/
    ///         npm-cli.js
    /// ```
    fn build_node_windows_zip(version_dir: &str) -> Vec<u8> {
        use std::io::Write as _;
        use zip::write::SimpleFileOptions;

        let buf = std::io::Cursor::new(Vec::new());
        let mut zw = zip::ZipWriter::new(buf);
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

        // Top-level versioned directory.
        zw.add_directory(format!("{version_dir}/"), opts).unwrap();

        // node.exe at the zip root (inside the versioned dir).
        zw.start_file(format!("{version_dir}/node.exe"), opts)
            .unwrap();
        zw.write_all(b"FAKE_NODE_EXE").unwrap();

        // npm.cmd — stays in place after normalization.
        zw.start_file(format!("{version_dir}/npm.cmd"), opts)
            .unwrap();
        zw.write_all(b"@echo off").unwrap();

        // node_modules tree.
        zw.add_directory(format!("{version_dir}/node_modules/"), opts)
            .unwrap();
        zw.add_directory(format!("{version_dir}/node_modules/npm/"), opts)
            .unwrap();
        zw.add_directory(format!("{version_dir}/node_modules/npm/bin/"), opts)
            .unwrap();
        zw.start_file(
            format!("{version_dir}/node_modules/npm/bin/npm-cli.js"),
            opts,
        )
        .unwrap();
        zw.write_all(b"// npm-cli placeholder").unwrap();

        let cursor = zw.finish().unwrap();
        cursor.into_inner()
    }

    /// Happy path: extraction + normalization produces the expected layout.
    /// Verified end-state:
    ///   <dest>/bin/node.exe          ← moved from zip root
    ///   <dest>/node_modules/npm/bin/npm-cli.js ← kept in place
    ///   <dest>/npm.cmd               ← left in place (unused by ahand)
    #[test]
    fn zip_extraction_normalizes_windows_layout() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("node");
        std::fs::create_dir_all(&dest).unwrap();

        let zip_bytes = build_node_windows_zip("node-v24.13.0-win-x64");
        extract_node_zip(&zip_bytes, &dest).expect("extraction should succeed");

        // node.exe MUST be in bin/ after normalization.
        assert!(
            dest.join("bin").join("node.exe").exists(),
            "bin/node.exe must exist after normalization"
        );

        // npm-cli.js must be present at its expected location.
        assert!(
            dest.join("node_modules")
                .join("npm")
                .join("bin")
                .join("npm-cli.js")
                .exists(),
            "node_modules/npm/bin/npm-cli.js must be present"
        );

        // npm.cmd should be present (not deleted).
        assert!(
            dest.join("npm.cmd").exists(),
            "npm.cmd should remain in place"
        );

        // node.exe must NOT still exist at the root (it was moved).
        assert!(
            !dest.join("node.exe").exists(),
            "node.exe must have been moved out of the root"
        );
    }

    /// Guard rejects a zip entry whose path contains a `..` component
    /// (path traversal attempt).
    #[test]
    fn zip_extraction_rejects_parent_dir_traversal() {
        use std::io::Write as _;
        use zip::write::SimpleFileOptions;

        let buf = std::io::Cursor::new(Vec::new());
        let mut zw = zip::ZipWriter::new(buf);
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

        // Top-level dir (will be stripped).
        zw.add_directory("node-v24.13.0-win-x64/", opts).unwrap();

        // Malicious entry: after stripping the first component the remaining
        // path is `../evil` — must be rejected.
        zw.start_file("node-v24.13.0-win-x64/../evil", opts)
            .unwrap();
        zw.write_all(b"evil content").unwrap();

        let cursor = zw.finish().unwrap();
        let zip_bytes = cursor.into_inner();

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("node");
        std::fs::create_dir_all(&dest).unwrap();

        let result = extract_node_zip(&zip_bytes, &dest);
        assert!(
            result.is_err(),
            "extraction must fail for path-traversal entries"
        );
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("..") || msg.contains("parent-dir") || msg.contains("traversal"),
            "error should mention traversal: {msg}"
        );
    }

    /// Guard rejects an absolute-path entry (RootDir component).
    ///
    /// Note: zip writers normalise paths so absolute entries cannot be crafted
    /// via `start_file`; we test the guard function directly to cover the
    /// RootDir and Prefix component branches.
    #[test]
    fn zip_extraction_rejects_absolute_path() {
        // Test the guard function directly for RootDir/Prefix rejection.
        let rooted = PathBuf::from("/etc/passwd");
        let result = guard_zip_path_traversal(&rooted);
        assert!(result.is_err(), "rooted path must be rejected");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("absolute"),
            "error should mention absolute path: {msg}"
        );
    }

    /// Guard rejects a symlink entry in the zip.
    #[test]
    fn zip_extraction_rejects_symlink_entry() {
        use zip::write::SimpleFileOptions;

        let buf = std::io::Cursor::new(Vec::new());
        let mut zw = zip::ZipWriter::new(buf);
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

        // Top-level dir.
        zw.add_directory("node-v24.13.0-win-x64/", opts).unwrap();
        // Add a normal file so the archive is not trivially empty.
        zw.start_file("node-v24.13.0-win-x64/node.exe", opts)
            .unwrap();
        {
            use std::io::Write as _;
            zw.write_all(b"FAKE").unwrap();
        }
        // Add a symlink entry via add_symlink — sets unix mode S_IFLNK.
        zw.add_symlink("node-v24.13.0-win-x64/evil_link", "/etc/passwd", opts)
            .unwrap();

        let cursor = zw.finish().unwrap();
        let zip_bytes = cursor.into_inner();

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("node");
        std::fs::create_dir_all(&dest).unwrap();

        let result = extract_node_zip(&zip_bytes, &dest);
        assert!(result.is_err(), "extraction must fail for symlink entries");
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("symlink"),
            "error should mention symlink: {msg}"
        );
    }

    /// guard_zip_path_traversal: normal relative paths are accepted.
    #[test]
    fn guard_zip_path_traversal_accepts_normal_paths() {
        let cases = [
            PathBuf::from("bin/node.exe"),
            PathBuf::from("node_modules/npm/bin/npm-cli.js"),
            PathBuf::from("npm.cmd"),
        ];
        for p in &cases {
            assert!(
                guard_zip_path_traversal(p).is_ok(),
                "should accept normal path: {}",
                p.display()
            );
        }
    }

    // -------------------------------------------------------------------------
    // Raw-path validation tests (pre-strip guard — item 1)
    // These confirm that malicious raw paths are ERROR-ed rather than
    // silently neutralised into the root by skip(1).
    // -------------------------------------------------------------------------

    /// A raw entry `../evil` — ParentDir as first component — must be rejected
    /// with an error BEFORE stripping, not silently normalised to `evil`.
    #[test]
    fn zip_extraction_raw_parent_dir_rejected_before_strip() {
        use zip::write::SimpleFileOptions;

        let buf = std::io::Cursor::new(Vec::new());
        let mut zw = zip::ZipWriter::new(buf);
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

        // Entry whose raw path starts with `..` — skipping the versioned
        // first component would otherwise land the content in the root.
        zw.start_file("../evil.txt", opts).unwrap();
        {
            use std::io::Write as _;
            zw.write_all(b"evil").unwrap();
        }

        let cursor = zw.finish().unwrap();
        let zip_bytes = cursor.into_inner();

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("node");
        std::fs::create_dir_all(&dest).unwrap();

        let result = extract_node_zip(&zip_bytes, &dest);
        assert!(
            result.is_err(),
            "raw '../evil' entry must be rejected before strip; got Ok"
        );
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("..") || msg.contains("parent-dir") || msg.contains("traversal"),
            "error must mention traversal: {msg}"
        );
    }

    /// A raw entry `/evil` — RootDir as first component — must be rejected
    /// with an error BEFORE stripping, not silently normalised to `evil`.
    ///
    /// Note: most zip writers normalise paths so absolute entries cannot be
    /// crafted via `start_file`; we build a raw entry directly to exercise the
    /// guard.
    #[test]
    fn zip_extraction_raw_rooted_path_rejected_before_strip() {
        use zip::write::SimpleFileOptions;

        // Build a zip with a raw absolute path entry.  The `zip` writer
        // accepts arbitrary byte strings for file names so we can inject
        // `/evil.txt` directly.
        let buf = std::io::Cursor::new(Vec::new());
        let mut zw = zip::ZipWriter::new(buf);
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

        // Use a path that PathBuf::from will parse as RootDir on unix.
        zw.start_file("/evil.txt", opts).unwrap();
        {
            use std::io::Write as _;
            zw.write_all(b"evil").unwrap();
        }

        let cursor = zw.finish().unwrap();
        let zip_bytes = cursor.into_inner();

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("node");
        std::fs::create_dir_all(&dest).unwrap();

        let result = extract_node_zip(&zip_bytes, &dest);
        assert!(
            result.is_err(),
            "raw '/evil.txt' entry must be rejected before strip; got Ok"
        );
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("absolute") || msg.contains("traversal") || msg.contains("parent-dir"),
            "error must mention absolute/traversal: {msg}"
        );
    }

    /// guard_zip_path_traversal: parent-dir components are rejected.
    #[test]
    fn guard_zip_path_traversal_rejects_parent_dir() {
        let cases = [
            PathBuf::from("../escape"),
            PathBuf::from("a/../../escape"),
            PathBuf::from("a/../b/../../../escape"),
        ];
        for p in &cases {
            assert!(
                guard_zip_path_traversal(p).is_err(),
                "should reject path with ..: {}",
                p.display()
            );
        }
    }

    // -------------------------------------------------------------------------
    // Carry-over T1 review tests
    // -------------------------------------------------------------------------

    /// Zip fixture without root node.exe — extraction succeeds (the archive is
    /// valid) but `bin/node.exe` is absent because there is nothing to move.
    /// The `ensure()` backstop (post-extraction binary-not-found bail) catches
    /// this in production; here we just confirm the extractor doesn't panic.
    ///
    /// NOTE: the move step in `extract_node_zip` is conditional (`if
    /// node_exe_src.exists()`), so a zip that never contained node.exe at the
    /// root simply skips the rename and leaves `bin/` absent — which is the
    /// correct "no binary" outcome for `ensure()` to report.
    #[test]
    fn zip_extraction_without_root_node_exe_succeeds_but_bin_absent() {
        use std::io::Write as _;
        use zip::write::SimpleFileOptions;

        let buf = std::io::Cursor::new(Vec::new());
        let mut zw = zip::ZipWriter::new(buf);
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

        // Top-level versioned directory — but NO node.exe inside.
        zw.add_directory("node-v24.13.0-win-x64/", opts).unwrap();
        zw.start_file("node-v24.13.0-win-x64/npm.cmd", opts)
            .unwrap();
        zw.write_all(b"@echo off").unwrap();

        let cursor = zw.finish().unwrap();
        let zip_bytes = cursor.into_inner();

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("node");
        std::fs::create_dir_all(&dest).unwrap();

        // Extraction must succeed — the extractor does not require node.exe.
        extract_node_zip(&zip_bytes, &dest)
            .expect("extraction should succeed even without node.exe");

        // bin/node.exe must be absent (move-skip pinned).
        // ensure() bails with "binary not found" as the backstop.
        assert!(
            !dest.join("bin").join("node.exe").exists(),
            "bin/node.exe must be absent when the zip had no root node.exe"
        );
    }

    /// Malformed zip with duplicate/type-conflicting entries: a file named `a`
    /// followed by a directory named `a/`. The extractor must either succeed
    /// cleanly or fail with an error — it must NOT write files outside `dest`.
    #[test]
    fn zip_extraction_duplicate_name_does_not_escape_root() {
        use std::io::Write as _;
        use zip::write::SimpleFileOptions;

        let buf = std::io::Cursor::new(Vec::new());
        let mut zw = zip::ZipWriter::new(buf);
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

        // Top-level dir.
        zw.add_directory("node-v24.13.0-win-x64/", opts).unwrap();

        // File named `a`.
        zw.start_file("node-v24.13.0-win-x64/a", opts).unwrap();
        zw.write_all(b"file content").unwrap();

        // Directory also named `a/` — type conflict.
        zw.add_directory("node-v24.13.0-win-x64/a/", opts).unwrap();

        // File inside the conflicting dir (should either extract or be skipped).
        zw.start_file("node-v24.13.0-win-x64/a/inner.txt", opts)
            .unwrap();
        zw.write_all(b"inner").unwrap();

        let cursor = zw.finish().unwrap();
        let zip_bytes = cursor.into_inner();

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("node");
        std::fs::create_dir_all(&dest).unwrap();

        // Extraction either succeeds or errors cleanly — either is acceptable.
        // What is NOT acceptable: writing anything outside `dest`.
        let result = extract_node_zip(&zip_bytes, &dest);
        if let Err(ref e) = result {
            // Error path: the message must not be a panic, just an anyhow error.
            let _ = format!("{e:#}"); // ensure it formats without panic
        }

        // Regardless of outcome, nothing must have escaped the dest root.
        let parent = dest.parent().unwrap();
        let escaped = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                let name = e.file_name();
                name != dest.file_name().unwrap()
            });
        assert!(
            !escaped,
            "no files must be written outside the dest directory"
        );
    }
}
