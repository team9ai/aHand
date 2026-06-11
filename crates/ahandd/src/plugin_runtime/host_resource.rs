use std::collections::BTreeMap;
use std::path::Path;

use crate::browser_setup::CheckStatus;

use super::builtin::builtin_registry;
use super::{
    HostResourceSnapshot, HostResourceValue, InstalledPluginResource, PluginManifest, PluginStatus,
    RuntimeDirs,
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
            node_resource(manifest, &runtime, &node_report).status,
        );
    }

    let mut plugins = Vec::new();
    for manifest in registry.manifests() {
        plugins.push(match manifest.id.as_str() {
            "shell" => shell_resource_from_path(manifest, shell_candidate.clone()),
            "file" => file_resource(manifest),
            "node" => node_resource(manifest, &runtime, &node_report),
            "python" => python_resource(manifest, &runtime).await,
            "browser-playwright-cli" => {
                browser_playwright_resource(manifest, &dependency_statuses).await
            }
            _ => manifest_resource(manifest, PluginStatus::Missing, BTreeMap::new()),
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
        return manifest_resource(manifest, PluginStatus::Missing, BTreeMap::new());
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

    manifest_resource(manifest, PluginStatus::Installed, resources)
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
    manifest_resource(manifest, PluginStatus::Installed, BTreeMap::new())
}

fn node_resource(
    manifest: &PluginManifest,
    runtime: &RuntimeDirs,
    report: &crate::browser_setup::CheckReport,
) -> InstalledPluginResource {
    let mut resources = BTreeMap::new();
    let status = match &report.status {
        CheckStatus::Ok { version, path, .. } => {
            // Use npm_invocation() to find the canonical "npm is available" file:
            // - Unix: node/bin/npm (the script itself)
            // - Windows: node_modules/npm/bin/npm-cli.js (the JS entry — node.exe
            //   is always present if node_bin() exists, so we check the JS file)
            let (npm_program, npm_leading) = runtime.npm_invocation();
            let npm_path = if npm_leading.is_empty() {
                // Unix: program IS the npm script.
                npm_program.clone()
            } else {
                // Windows: first leading arg is npm-cli.js.
                std::path::PathBuf::from(&npm_leading[0])
            };
            resources.insert(
                "node".to_string(),
                HostResourceValue::Executable {
                    name: "node".to_string(),
                    path: path_to_string(path),
                    version: Some(version.clone()),
                },
            );
            if npm_path.is_file() {
                resources.insert(
                    "npm".to_string(),
                    HostResourceValue::Executable {
                        name: "npm".to_string(),
                        path: path_to_string(&npm_path),
                        version: None,
                    },
                );
                PluginStatus::Installed
            } else {
                PluginStatus::Failed
            }
        }
        other => plugin_status_from_check(other),
    };

    manifest_resource(manifest, status, resources)
}

async fn python_resource(
    manifest: &PluginManifest,
    runtime: &RuntimeDirs,
) -> InstalledPluginResource {
    let python = runtime.python_bin();
    if !python.exists() {
        return manifest_resource(manifest, PluginStatus::Missing, BTreeMap::new());
    }

    let Ok(version) = executable_version(&python).await else {
        return manifest_resource(manifest, PluginStatus::Failed, BTreeMap::new());
    };

    let mut resources = BTreeMap::new();
    resources.insert(
        "python".to_string(),
        HostResourceValue::Executable {
            name: "python".to_string(),
            path: path_to_string(python),
            version,
        },
    );

    manifest_resource(manifest, PluginStatus::Installed, resources)
}

async fn browser_playwright_resource(
    manifest: &PluginManifest,
    dependency_statuses: &BTreeMap<String, PluginStatus>,
) -> InstalledPluginResource {
    if !dependencies_installed(manifest, dependency_statuses) {
        return manifest_resource(manifest, PluginStatus::Blocked, BTreeMap::new());
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

    manifest_resource(manifest, status, resources)
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

fn manifest_resource(
    manifest: &PluginManifest,
    status: PluginStatus,
    resources: BTreeMap<String, HostResourceValue>,
) -> InstalledPluginResource {
    InstalledPluginResource {
        id: manifest.id.clone(),
        version: manifest.version.clone(),
        status,
        dependencies: manifest.dependencies.clone(),
        capabilities: manifest.capabilities.clone(),
        resources,
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

    #[test]
    fn node_resource_does_not_report_missing_npm_executable() {
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

        let plugin = node_resource(&manifest("node"), &runtime, &report);

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
}
