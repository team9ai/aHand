use std::collections::BTreeMap;
use std::path::Path;

use crate::browser_setup::CheckStatus;
use serde::Deserialize;
use tokio::process::Command;

use super::builtin::builtin_registry;
use super::{
    HostResourceSnapshot, HostResourceValue, InstalledPluginResource, PluginManifest, PluginStatus,
    RuntimeDirs, RuntimePackage,
};

pub async fn get_host_resource() -> anyhow::Result<HostResourceSnapshot> {
    let registry = builtin_registry()?;
    let runtime = RuntimeDirs::new()?;
    let node_report = crate::browser_setup::node::inspect().await;
    let shell_candidate = shell_path();
    let mut dependency_statuses = BTreeMap::new();
    dependency_statuses.insert(
        "shell".to_string(),
        shell_status_from_path(&shell_candidate),
    );
    if let Some(manifest) = registry.get("node") {
        dependency_statuses.insert(
            "node".to_string(),
            node_resource(manifest, &runtime, &node_report).await.status,
        );
    }

    let mut plugins = Vec::new();
    for manifest in registry.manifests() {
        plugins.push(match manifest.id.as_str() {
            "shell" => shell_resource_from_path(manifest, shell_candidate.clone()),
            "file" => file_resource(manifest),
            "node" => node_resource(manifest, &runtime, &node_report).await,
            "python" => python_resource(manifest, &runtime).await,
            "browser-playwright-cli" => {
                browser_playwright_resource(manifest, &dependency_statuses).await
            }
            _ => manifest_resource(manifest, PluginStatus::Missing, BTreeMap::new(), Vec::new()),
        });
    }

    Ok(HostResourceSnapshot {
        runtime_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: host_platform(),
        arch: host_arch(),
        plugins,
    })
}

fn host_platform() -> String {
    normalize_platform(std::env::consts::OS).to_string()
}

fn normalize_platform(platform: &str) -> &str {
    match platform {
        "macos" => "darwin",
        other => other,
    }
}

fn host_arch() -> String {
    normalize_arch(std::env::consts::ARCH).to_string()
}

fn normalize_arch(arch: &str) -> &str {
    match arch {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    }
}

fn shell_path() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            if cfg!(windows) {
                std::env::var("COMSPEC")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            } else {
                None
            }
        })
        .unwrap_or_else(|| {
            if cfg!(windows) {
                "cmd.exe".to_string()
            } else {
                "/bin/sh".to_string()
            }
        })
}

fn shell_resource_from_path(manifest: &PluginManifest, shell: String) -> InstalledPluginResource {
    if shell_status_from_path(&shell) != PluginStatus::Installed {
        return manifest_resource(manifest, PluginStatus::Missing, BTreeMap::new(), Vec::new());
    }

    let mut resources = BTreeMap::new();
    resources.insert(
        "shell".to_string(),
        HostResourceValue::Executable {
            name: "shell".to_string(),
            path: shell,
            version: None,
        },
    );

    manifest_resource(manifest, PluginStatus::Installed, resources, Vec::new())
}

fn shell_status_from_path(shell: &str) -> PluginStatus {
    if is_executable_file(Path::new(shell)) {
        PluginStatus::Installed
    } else {
        PluginStatus::Missing
    }
}

fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        path.metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }

    #[cfg(not(unix))]
    {
        true
    }
}

fn file_resource(manifest: &PluginManifest) -> InstalledPluginResource {
    manifest_resource(
        manifest,
        PluginStatus::Installed,
        BTreeMap::new(),
        Vec::new(),
    )
}

async fn node_resource(
    manifest: &PluginManifest,
    runtime: &RuntimeDirs,
    report: &crate::browser_setup::CheckReport,
) -> InstalledPluginResource {
    let mut resources = BTreeMap::new();
    let mut packages = Vec::new();
    let status = match &report.status {
        CheckStatus::Ok { version, path, .. } => {
            let npm = runtime.npm_bin();
            resources.insert(
                "node".to_string(),
                HostResourceValue::Executable {
                    name: "node".to_string(),
                    path: path_to_string(path),
                    version: Some(version.clone()),
                },
            );
            if npm.is_file() {
                resources.insert(
                    "npm".to_string(),
                    HostResourceValue::Executable {
                        name: "npm".to_string(),
                        path: path_to_string(npm),
                        version: None,
                    },
                );
                packages = node_packages(runtime).await;
                PluginStatus::Installed
            } else {
                PluginStatus::Failed
            }
        }
        other => plugin_status_from_check(other),
    };

    manifest_resource(manifest, status, resources, packages)
}

async fn python_resource(
    manifest: &PluginManifest,
    runtime: &RuntimeDirs,
) -> InstalledPluginResource {
    let python = runtime.python_bin();
    if !python.exists() {
        return manifest_resource(manifest, PluginStatus::Missing, BTreeMap::new(), Vec::new());
    }

    let Ok(version) = executable_version(&python).await else {
        return manifest_resource(manifest, PluginStatus::Failed, BTreeMap::new(), Vec::new());
    };

    let mut resources = BTreeMap::new();
    resources.insert(
        "python".to_string(),
        HostResourceValue::Executable {
            name: "python".to_string(),
            path: path_to_string(&python),
            version,
        },
    );

    let packages = python_packages(&python).await;
    manifest_resource(manifest, PluginStatus::Installed, resources, packages)
}

async fn browser_playwright_resource(
    manifest: &PluginManifest,
    dependency_statuses: &BTreeMap<String, PluginStatus>,
) -> InstalledPluginResource {
    if !dependencies_installed(manifest, dependency_statuses) {
        return manifest_resource(manifest, PluginStatus::Blocked, BTreeMap::new(), Vec::new());
    }

    let report = crate::browser_setup::playwright::inspect().await;
    let mut resources = BTreeMap::new();
    let status = match &report.status {
        CheckStatus::Ok { version, path, .. } => {
            resources.insert(
                "playwrightCli".to_string(),
                HostResourceValue::Executable {
                    name: "playwright-cli".to_string(),
                    path: path_to_string(path),
                    version: Some(version.clone()),
                },
            );
            PluginStatus::Installed
        }
        other => plugin_status_from_check(other),
    };

    manifest_resource(manifest, status, resources, Vec::new())
}

fn dependencies_installed(
    manifest: &PluginManifest,
    dependency_statuses: &BTreeMap<String, PluginStatus>,
) -> bool {
    manifest
        .dependencies
        .iter()
        .all(|dependency| dependency_statuses.get(dependency) == Some(&PluginStatus::Installed))
}

async fn node_packages(runtime: &RuntimeDirs) -> Vec<RuntimePackage> {
    let npm = runtime.npm_bin();
    if !npm.is_file() {
        return Vec::new();
    }
    let Ok(output) = Command::new(npm)
        .args(["list", "-g", "--depth=0", "--json"])
        .output()
        .await
    else {
        return Vec::new();
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_npm_packages(&stdout)
}

async fn python_packages(python: &Path) -> Vec<RuntimePackage> {
    let Ok(output) = Command::new(python)
        .args(["-m", "pip", "list", "--format=json"])
        .output()
        .await
    else {
        return Vec::new();
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_python_packages(&stdout)
}

#[derive(Debug, Deserialize)]
struct NpmList {
    dependencies: Option<BTreeMap<String, NpmDependency>>,
}

#[derive(Debug, Deserialize)]
struct NpmDependency {
    version: Option<String>,
}

fn parse_npm_packages(json: &str) -> Vec<RuntimePackage> {
    let Ok(list) = serde_json::from_str::<NpmList>(json) else {
        return Vec::new();
    };
    let mut packages = list
        .dependencies
        .unwrap_or_default()
        .into_iter()
        .filter(|(name, _)| !is_node_system_package(name))
        .map(|(name, dep)| RuntimePackage {
            name,
            version: dep.version,
        })
        .collect::<Vec<_>>();
    sort_packages(&mut packages);
    packages
}

#[derive(Debug, Deserialize)]
struct PipListPackage {
    name: String,
    version: Option<String>,
}

fn parse_python_packages(json: &str) -> Vec<RuntimePackage> {
    let Ok(list) = serde_json::from_str::<Vec<PipListPackage>>(json) else {
        return Vec::new();
    };
    let mut packages = list
        .into_iter()
        .filter(|pkg| !is_python_system_package(&pkg.name))
        .map(|pkg| RuntimePackage {
            name: pkg.name,
            version: pkg.version,
        })
        .collect::<Vec<_>>();
    sort_packages(&mut packages);
    packages
}

fn is_node_system_package(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "npm" | "corepack")
}

fn is_python_system_package(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "pip" | "setuptools" | "wheel"
    )
}

fn sort_packages(packages: &mut [RuntimePackage]) {
    packages.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
}

fn manifest_resource(
    manifest: &PluginManifest,
    status: PluginStatus,
    resources: BTreeMap<String, HostResourceValue>,
    packages: Vec<RuntimePackage>,
) -> InstalledPluginResource {
    InstalledPluginResource {
        id: manifest.id.clone(),
        version: manifest.version.clone(),
        status,
        dependencies: manifest.dependencies.clone(),
        capabilities: manifest.capabilities.clone(),
        resources,
        packages,
        help_prompt: manifest.help.as_ref().map(|help| help.prompt.clone()),
    }
}

fn plugin_status_from_check(status: &CheckStatus) -> PluginStatus {
    match status {
        CheckStatus::Ok { .. } => PluginStatus::Installed,
        CheckStatus::Missing | CheckStatus::NoneDetected { .. } => PluginStatus::Missing,
        CheckStatus::Outdated { .. } => PluginStatus::Outdated,
        CheckStatus::Failed { .. } => PluginStatus::Failed,
    }
}

async fn executable_version(path: &Path) -> anyhow::Result<Option<String>> {
    let output = tokio::process::Command::new(path)
        .arg("--version")
        .output()
        .await?;
    if !output.status.success() {
        anyhow::bail!("{} --version exited with {}", path.display(), output.status);
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        return Ok(Some(stdout));
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        Ok(None)
    } else {
        Ok(Some(stderr))
    }
}

fn path_to_string(path: impl AsRef<Path>) -> String {
    path.as_ref().to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser_setup::{CheckReport, CheckSource, CheckStatus};
    use std::path::PathBuf;

    fn manifest(id: &str) -> PluginManifest {
        PluginManifest {
            id: id.to_string(),
            version: "0.1.0".to_string(),
            display_name: id.to_string(),
            dependencies: Vec::new(),
            capabilities: Vec::new(),
            resources: Default::default(),
            help: None,
        }
    }

    fn manifest_with_dependencies(id: &str, dependencies: &[&str]) -> PluginManifest {
        let mut manifest = manifest(id);
        manifest.dependencies = dependencies
            .iter()
            .map(|dependency| dependency.to_string())
            .collect();
        manifest
    }

    #[tokio::test]
    async fn snapshot_contains_first_party_plugins() {
        let snapshot = get_host_resource().await.unwrap();
        let ids: Vec<_> = snapshot
            .plugins
            .iter()
            .map(|plugin| plugin.id.as_str())
            .collect();

        assert_eq!(
            ids,
            vec!["browser-playwright-cli", "file", "node", "python", "shell"]
        );
    }

    #[tokio::test]
    async fn browser_plugin_reports_shell_and_node_dependencies() {
        let snapshot = get_host_resource().await.unwrap();
        let plugin = snapshot
            .plugins
            .iter()
            .find(|plugin| plugin.id == "browser-playwright-cli")
            .unwrap();

        assert_eq!(plugin.dependencies, vec!["shell", "node"]);
        assert_eq!(plugin.capabilities, vec!["browser"]);
        assert!(
            plugin
                .help_prompt
                .as_ref()
                .is_some_and(|prompt| prompt.contains("browser automation"))
        );
    }

    #[test]
    fn platform_and_arch_use_public_host_resource_vocabulary() {
        assert_eq!(normalize_platform("macos"), "darwin");
        assert_eq!(normalize_platform("linux"), "linux");
        assert_eq!(normalize_platform("windows"), "windows");
        assert_eq!(normalize_arch("aarch64"), "arm64");
        assert_eq!(normalize_arch("x86_64"), "x64");
    }

    #[tokio::test]
    async fn node_resource_does_not_report_missing_npm_executable() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = RuntimeDirs::from_root(dir.path().to_path_buf());
        let report = CheckReport {
            name: "node",
            label: "Node.js",
            status: CheckStatus::Ok {
                version: "24.13.0".to_string(),
                path: PathBuf::from("/tmp/node"),
                source: CheckSource::Managed,
            },
            fix_hint: None,
        };

        let plugin = node_resource(&manifest("node"), &runtime, &report).await;

        assert_eq!(plugin.status, PluginStatus::Failed);
        assert!(plugin.resources.contains_key("node"));
        assert!(!plugin.resources.contains_key("npm"));
    }

    #[tokio::test]
    async fn python_resource_fails_when_path_is_not_executable() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = RuntimeDirs::from_root(dir.path().to_path_buf());
        std::fs::create_dir_all(runtime.python_bin()).unwrap();

        let plugin = python_resource(&manifest("python"), &runtime).await;

        assert_eq!(plugin.status, PluginStatus::Failed);
        assert!(plugin.resources.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn shell_resource_missing_when_candidate_is_not_executable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let shell = dir.path().join("shell");
        std::fs::write(&shell, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o644)).unwrap();

        let plugin = shell_resource_from_path(&manifest("shell"), path_to_string(shell));

        assert_eq!(plugin.status, PluginStatus::Missing);
        assert!(plugin.resources.is_empty());
    }

    #[test]
    fn shell_resource_missing_when_candidate_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing-shell");

        let plugin = shell_resource_from_path(&manifest("shell"), path_to_string(missing));

        assert_eq!(plugin.status, PluginStatus::Missing);
        assert!(plugin.resources.is_empty());
    }

    #[tokio::test]
    async fn browser_plugin_blocks_when_shell_dependency_is_missing() {
        let manifest = manifest_with_dependencies("browser-playwright-cli", &["shell", "node"]);
        let dependency_statuses = BTreeMap::from([
            ("shell".to_string(), PluginStatus::Missing),
            ("node".to_string(), PluginStatus::Installed),
        ]);

        let plugin = browser_playwright_resource(&manifest, &dependency_statuses).await;

        assert_eq!(plugin.status, PluginStatus::Blocked);
        assert!(plugin.resources.is_empty());
    }

    #[test]
    fn parse_npm_packages_filters_managed_runtime_system_packages() {
        let packages = parse_npm_packages(
            r#"{
              "dependencies": {
                "npm": {"version": "11.0.0"},
                "corepack": {"version": "0.31.0"},
                "@playwright/cli": {"version": "1.56.1"},
                "typescript": {"version": "5.9.3"}
              }
            }"#,
        );

        assert_eq!(
            packages,
            vec![
                RuntimePackage {
                    name: "@playwright/cli".to_string(),
                    version: Some("1.56.1".to_string()),
                },
                RuntimePackage {
                    name: "typescript".to_string(),
                    version: Some("5.9.3".to_string()),
                },
            ]
        );
    }

    #[test]
    fn parse_python_packages_filters_bootstrap_packages() {
        let packages = parse_python_packages(
            r#"[
              {"name": "pip", "version": "25.0"},
              {"name": "setuptools", "version": "75.0"},
              {"name": "wheel", "version": "0.45"},
              {"name": "pytest", "version": "9.0.1"},
              {"name": "requests", "version": "2.32.5"}
            ]"#,
        );

        assert_eq!(
            packages,
            vec![
                RuntimePackage {
                    name: "pytest".to_string(),
                    version: Some("9.0.1".to_string()),
                },
                RuntimePackage {
                    name: "requests".to_string(),
                    version: Some("2.32.5".to_string()),
                },
            ]
        );
    }
}
