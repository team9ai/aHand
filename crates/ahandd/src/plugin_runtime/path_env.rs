use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tracing::warn;

use crate::plugin_runtime::{HostResourceValue, InstalledPluginResource, PluginStatus};

const MANAGED_RUNTIME_EXECUTABLES: &[(&str, &str)] = &[("node", "node"), ("python", "python")];

pub fn runtime_path_dirs(plugins: &[InstalledPluginResource]) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    for (plugin_id, resource_key) in MANAGED_RUNTIME_EXECUTABLES {
        let Some(plugin) = plugins
            .iter()
            .find(|plugin| plugin.id == *plugin_id && plugin.status == PluginStatus::Installed)
        else {
            continue;
        };
        let Some(HostResourceValue::Executable { path, .. }) = plugin.resources.get(*resource_key)
        else {
            continue;
        };
        let Some(parent) = Path::new(path).parent() else {
            continue;
        };

        push_unique_path(&mut dirs, parent.to_path_buf());
    }

    dirs
}

pub fn prepend_path_dirs(base_path: &str, dirs: &[PathBuf]) -> String {
    let mut paths = Vec::new();
    for dir in dirs {
        push_unique_path(&mut paths, dir.clone());
    }

    if !base_path.is_empty() {
        for existing in std::env::split_paths(base_path) {
            push_unique_path(&mut paths, existing);
        }
    }

    std::env::join_paths(&paths)
        .map(|joined| joined.to_string_lossy().into_owned())
        .unwrap_or_else(|_| fallback_join(base_path, dirs))
}

pub async fn path_with_installed_runtime_bins(base_path: &str) -> anyhow::Result<String> {
    let snapshot = super::get_host_resource().await?;
    let runtime_dirs = runtime_path_dirs(&snapshot.plugins);
    Ok(prepend_path_dirs(base_path, &runtime_dirs))
}

pub async fn path_with_installed_runtime_bins_or_base(base_path: &str) -> String {
    match path_with_installed_runtime_bins(base_path).await {
        Ok(path) => path,
        Err(error) => {
            warn!(%error, "failed to resolve managed runtime PATH entries");
            base_path.to_string()
        }
    }
}

pub fn is_path_env_key(key: &str) -> bool {
    key.eq_ignore_ascii_case("PATH")
}

pub fn path_env_value(envs: &HashMap<String, String>) -> Option<&str> {
    envs.get("PATH").map(String::as_str).or_else(|| {
        envs.iter()
            .find(|(key, _)| is_path_env_key(key))
            .map(|(_, value)| value.as_str())
    })
}

pub async fn child_process_path(envs: &HashMap<String, String>) -> String {
    let base_path = path_env_value(envs)
        .map(str::to_string)
        .unwrap_or_else(|| std::env::var("PATH").unwrap_or_default());
    path_with_installed_runtime_bins_or_base(&base_path).await
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if path.as_os_str().is_empty() {
        return;
    }
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn fallback_join(base_path: &str, dirs: &[PathBuf]) -> String {
    let separator = if cfg!(windows) { ";" } else { ":" };
    let mut parts: Vec<String> = Vec::new();

    for dir in dirs {
        let value = dir.to_string_lossy();
        if !value.is_empty() && !parts.iter().any(|part| part == value.as_ref()) {
            parts.push(value.into_owned());
        }
    }

    if !base_path.is_empty() {
        parts.push(base_path.to_string());
    }

    parts.join(separator)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use crate::plugin_runtime::{HostResourceValue, InstalledPluginResource, PluginStatus};

    fn plugin(
        id: &str,
        status: PluginStatus,
        resources: BTreeMap<String, HostResourceValue>,
    ) -> InstalledPluginResource {
        InstalledPluginResource {
            id: id.to_string(),
            version: "0.1.0".to_string(),
            status,
            dependencies: Vec::new(),
            capabilities: Vec::new(),
            resources,
            packages: Vec::new(),
            help_prompt: None,
        }
    }

    fn executable(key: &str, path: &str) -> BTreeMap<String, HostResourceValue> {
        let mut resources = BTreeMap::new();
        resources.insert(
            key.to_string(),
            HostResourceValue::Executable {
                name: key.to_string(),
                path: path.to_string(),
                version: Some("test".to_string()),
            },
        );
        resources
    }

    #[test]
    fn runtime_path_dirs_uses_installed_node_and_python_executables() {
        let plugins = vec![
            plugin(
                "node",
                PluginStatus::Installed,
                executable("node", "/runtime/dependencies/node/bin/node"),
            ),
            plugin(
                "python",
                PluginStatus::Installed,
                executable("python", "/runtime/dependencies/python/bin/python3"),
            ),
            plugin(
                "browser-playwright-cli",
                PluginStatus::Installed,
                executable("playwright", "/runtime/dependencies/node/bin/playwright"),
            ),
            plugin(
                "node",
                PluginStatus::Missing,
                executable("node", "/ignored/node/bin/node"),
            ),
        ];

        let dirs = super::runtime_path_dirs(&plugins);

        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/runtime/dependencies/node/bin"),
                PathBuf::from("/runtime/dependencies/python/bin"),
            ]
        );
    }

    #[test]
    fn prepend_path_dirs_adds_runtime_bins_before_existing_path_and_deduplicates() {
        let base_path = std::env::join_paths([
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/runtime/dependencies/node/bin"),
            PathBuf::from("/usr/bin"),
        ])
        .unwrap()
        .to_string_lossy()
        .into_owned();
        let dirs = vec![
            PathBuf::from("/runtime/dependencies/node/bin"),
            PathBuf::from("/runtime/dependencies/python/bin"),
            PathBuf::from("/runtime/dependencies/node/bin"),
        ];

        let actual = super::prepend_path_dirs(&base_path, &dirs);
        let actual_dirs: Vec<PathBuf> = std::env::split_paths(&actual).collect();

        assert_eq!(
            actual_dirs,
            vec![
                PathBuf::from("/runtime/dependencies/node/bin"),
                PathBuf::from("/runtime/dependencies/python/bin"),
                PathBuf::from("/usr/local/bin"),
                PathBuf::from("/usr/bin"),
            ]
        );
    }

    #[test]
    fn path_env_value_accepts_case_variants() {
        let mut envs = std::collections::HashMap::new();
        envs.insert("Path".to_string(), "/custom/bin".to_string());

        assert_eq!(super::path_env_value(&envs), Some("/custom/bin"));
    }

    #[test]
    fn path_env_value_prefers_canonical_path_key() {
        let mut envs = std::collections::HashMap::new();
        envs.insert("Path".to_string(), "/custom/bin".to_string());
        envs.insert("PATH".to_string(), "/canonical/bin".to_string());

        assert_eq!(super::path_env_value(&envs), Some("/canonical/bin"));
    }
}
