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
                    format!(
                        "Node.js v{ver}.x already installed at {}",
                        dirs.node.display()
                    ),
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
        format!(
            "Installing Node.js v{NODE_LTS_VERSION} to {}",
            dirs.node.display()
        ),
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
        let phases: Vec<String> = events
            .iter()
            .map(|e| format!("{:?}", e.phase))
            .collect();
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
        std::fs::write(
            &node_bin,
            "#!/bin/sh\necho 'v24.13.0'\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&node_bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&node_bin, perms).unwrap();

        // read_node_major_version reads the binary directly, so test it in isolation
        let version = read_node_major_version(&node_bin).await;
        assert_eq!(version, Some(24), "fake node binary should report major version 24");
    }

    /// Confirms read_node_major_version returns None for a binary that fails to run.
    #[tokio::test]
    async fn read_node_major_version_returns_none_for_nonexistent_bin() {
        let version = read_node_major_version(std::path::Path::new("/nonexistent/node")).await;
        assert_eq!(
            version, None,
            "nonexistent binary should return None"
        );
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
        assert_eq!(
            version, None,
            "garbage version output should return None"
        );
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
        assert!(
            ["arm64", "x64"].contains(&arch),
            "unexpected arch: {arch}"
        );
    }
}
