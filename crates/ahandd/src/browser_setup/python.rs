use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::types::{CheckReport, CheckSource, CheckStatus, FixHint, Phase, ProgressEvent};

pub const PYTHON_MIN_MAJOR: u32 = 3;
pub const PYTHON_MIN_MINOR: u32 = 12;
pub const PYTHON_VERSION: &str = "3.12.13";
pub const PYTHON_BUILD_RELEASE: &str = "20260510";

pub struct Dirs {
    pub python: PathBuf,
    runtime: crate::plugin_runtime::RuntimeDirs,
}

impl Dirs {
    pub fn new() -> Result<Self> {
        let runtime = crate::plugin_runtime::RuntimeDirs::new()?;
        Ok(Self::from_runtime(runtime))
    }

    #[cfg(test)]
    pub fn from_runtime_root(root: PathBuf) -> Self {
        Self::from_runtime(crate::plugin_runtime::RuntimeDirs::from_root(root))
    }

    fn from_runtime(runtime: crate::plugin_runtime::RuntimeDirs) -> Self {
        let python = runtime.python_dir();
        Self { python, runtime }
    }

    pub fn local_python_bin(&self) -> PathBuf {
        self.runtime.python_bin()
    }
}

fn local_python_bin() -> Result<PathBuf> {
    Ok(Dirs::new()?.local_python_bin())
}

pub async fn inspect() -> CheckReport {
    let report = async {
        let bin = local_python_bin()?;
        if !bin.exists() {
            return Ok::<CheckReport, anyhow::Error>(missing_report());
        }

        match read_python_version(&bin).await {
            Some(version) if version.meets_minimum() => Ok(CheckReport {
                name: "python",
                label: "Python",
                status: CheckStatus::Ok {
                    version: version.to_string(),
                    path: bin,
                    source: CheckSource::Managed,
                },
                fix_hint: None,
            }),
            Some(version) => Ok(CheckReport {
                name: "python",
                label: "Python",
                status: CheckStatus::Outdated {
                    current: version.to_string(),
                    required: format!("Python {PYTHON_MIN_MAJOR}.{PYTHON_MIN_MINOR}"),
                    path: bin,
                },
                fix_hint: Some(FixHint::RunStep {
                    command: "ahandd plugin install python --force".into(),
                }),
            }),
            None => Ok(CheckReport {
                name: "python",
                label: "Python",
                status: CheckStatus::Missing,
                fix_hint: Some(FixHint::RunStep {
                    command: "ahandd plugin install python --force".into(),
                }),
            }),
        }
    }
    .await;

    report.unwrap_or_else(|_| missing_report())
}

pub async fn ensure(
    force: bool,
    progress: &(dyn Fn(ProgressEvent) + Send + Sync),
) -> Result<CheckReport> {
    let dirs = Dirs::new()?;
    let local_python = dirs.local_python_bin();

    if !force
        && local_python.exists()
        && let Some(version) = read_python_version(&local_python).await
        && version.meets_minimum()
    {
        emit(
            progress,
            Phase::Done,
            format!("{} already installed at {}", version, dirs.python.display()),
        );
        return Ok(inspect().await);
    }

    if dirs.python.exists() {
        let _ = std::fs::remove_dir_all(&dirs.python);
    }

    emit(
        progress,
        Phase::Starting,
        format!(
            "Installing Python {PYTHON_VERSION} to {}",
            dirs.python.display()
        ),
    );

    install_python(&dirs, progress).await.context(
        "Failed to install Python. Check your network connection and retry, \
         or install Python manually at the managed runtime path.",
    )?;

    if !local_python.exists() {
        anyhow::bail!(
            "Python installation completed but binary not found at {}.",
            local_python.display()
        );
    }

    let Some(version) = read_python_version(&local_python).await else {
        anyhow::bail!(
            "Python installation completed but version check failed at {}.",
            local_python.display()
        );
    };
    if !version.meets_minimum() {
        anyhow::bail!(
            "Python installation completed but version {} is below required Python {}.{}.",
            version,
            PYTHON_MIN_MAJOR,
            PYTHON_MIN_MINOR
        );
    }

    emit(
        progress,
        Phase::Done,
        format!("Python {PYTHON_VERSION} ready at {}", dirs.python.display()),
    );

    Ok(inspect().await)
}

async fn install_python(
    dirs: &Dirs,
    progress: &(dyn Fn(ProgressEvent) + Send + Sync),
) -> Result<()> {
    let asset = python_asset_name()?;
    let url = format!(
        "https://github.com/astral-sh/python-build-standalone/releases/download/{PYTHON_BUILD_RELEASE}/{asset}"
    );

    emit(progress, Phase::Downloading, format!("Downloading {asset}"));

    let bytes = download_bytes(&url).await.context(format!(
        "Failed to download Python from {url} - check your network connection"
    ))?;

    std::fs::create_dir_all(&dirs.python).context(format!(
        "Failed to create directory {}: permission denied or disk full",
        dirs.python.display()
    ))?;

    emit(
        progress,
        Phase::Extracting,
        "Extracting Python archive".into(),
    );

    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    archive.set_preserve_permissions(true);
    for entry in archive
        .entries()
        .context("Failed to read Python archive - download may be corrupted")?
    {
        let mut entry = entry.context("Corrupted entry in Python archive")?;
        let path = entry.path()?.into_owned();
        let Some(dest) = archive_destination(&dirs.python, &path) else {
            continue;
        };
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry.unpack(&dest).context(format!(
            "Failed to extract {} - disk may be full",
            dest.display()
        ))?;
    }

    emit(
        progress,
        Phase::Verifying,
        "Verifying Python installation".into(),
    );

    Ok(())
}

fn archive_destination(root: &Path, archive_path: &Path) -> Option<PathBuf> {
    let mut components = archive_path.components();
    if !matches!(components.next()?, Component::Normal(_)) {
        return None;
    }

    let mut dest = root.to_path_buf();
    let mut has_child = false;
    for component in components {
        match component {
            Component::Normal(part) => {
                dest.push(part);
                has_child = true;
            }
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => return None,
        }
    }

    has_child.then_some(dest)
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

fn python_asset_name() -> Result<String> {
    let target = python_target_triple()?;
    Ok(format!(
        "cpython-{PYTHON_VERSION}+{PYTHON_BUILD_RELEASE}-{target}-install_only.tar.gz"
    ))
}

fn python_target_triple() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        (os, arch) => bail!("unsupported Python runtime platform {os}/{arch}"),
    }
}

async fn read_python_version(python_bin: &Path) -> Option<PythonVersion> {
    let output = tokio::process::Command::new(python_bin)
        .arg("--version")
        .output()
        .await
        .ok()?;
    let mut version_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if version_str.is_empty() {
        version_str = String::from_utf8_lossy(&output.stderr).trim().to_string();
    }
    parse_python_version(&version_str)
}

fn parse_python_version(value: &str) -> Option<PythonVersion> {
    let version = value.trim().strip_prefix("Python ")?;
    let mut parts = version.split_whitespace().next()?.split('.');
    Some(PythonVersion {
        major: parts.next()?.parse().ok()?,
        minor: parts.next()?.parse().ok()?,
        patch: parts.next().and_then(|part| part.parse().ok()).unwrap_or(0),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PythonVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

impl PythonVersion {
    fn meets_minimum(self) -> bool {
        (self.major, self.minor) >= (PYTHON_MIN_MAJOR, PYTHON_MIN_MINOR)
    }
}

impl std::fmt::Display for PythonVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Python {}.{}.{}", self.major, self.minor, self.patch)
    }
}

fn missing_report() -> CheckReport {
    CheckReport {
        name: "python",
        label: "Python",
        status: CheckStatus::Missing,
        fix_hint: Some(FixHint::RunStep {
            command: "ahandd plugin install python".into(),
        }),
    }
}

fn emit(progress: &(dyn Fn(ProgressEvent) + Send + Sync), phase: Phase, message: String) {
    progress(ProgressEvent {
        step: "python",
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
    fn dirs_use_plugin_runtime_python_directory() {
        let root = PathBuf::from("/tmp/ahand-primary-runtime");
        let dirs = Dirs::from_runtime_root(root);

        assert_eq!(
            dirs.python,
            PathBuf::from("/tmp/ahand-primary-runtime/dependencies/python")
        );
        assert_eq!(
            dirs.local_python_bin(),
            PathBuf::from("/tmp/ahand-primary-runtime/dependencies/python/bin/python3")
        );
    }

    #[test]
    fn python_asset_name_uses_install_only_archive_for_current_platform() {
        let asset = python_asset_name().unwrap();

        assert!(asset.starts_with("cpython-3.12.13+20260510-"));
        assert!(asset.ends_with("-install_only.tar.gz"));
    }

    #[test]
    fn parse_python_version_accepts_standard_output() {
        assert_eq!(
            parse_python_version("Python 3.12.13"),
            Some(PythonVersion {
                major: 3,
                minor: 12,
                patch: 13
            })
        );
    }

    #[test]
    fn archive_destination_rejects_parent_components() {
        let root = PathBuf::from("/tmp/ahand-primary-runtime/dependencies/python");

        assert_eq!(
            archive_destination(&root, Path::new("python/bin/python3")),
            Some(root.join("bin").join("python3"))
        );
        assert_eq!(
            archive_destination(&root, Path::new("python/../evil")),
            None
        );
        assert_eq!(
            archive_destination(&root, Path::new("/python/bin/python3")),
            None
        );
    }

    #[test]
    fn emit_produces_python_step_events() {
        let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let cb_events = events.clone();
        let cb = move |e: ProgressEvent| {
            cb_events.lock().unwrap().push(e);
        };

        emit(&cb, Phase::Starting, "Starting Python install".into());
        emit(&cb, Phase::Downloading, "Downloading tarball".into());
        emit(&cb, Phase::Extracting, "Extracting archive".into());
        emit(&cb, Phase::Verifying, "Verifying install".into());
        emit(&cb, Phase::Done, "Python ready".into());

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 5);
        for event in events.iter() {
            assert_eq!(event.step, "python");
            assert!(event.stream.is_none());
            assert!(event.percent.is_none());
        }
    }
}
