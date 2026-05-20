use std::collections::BTreeSet;
use std::sync::Arc;

use crate::browser_setup::{self, CheckReport, ProgressEvent};

use super::PluginRegistry;
use super::builtin::builtin_registry;

pub type InstallProgress = Arc<dyn Fn(ProgressEvent) + Send + Sync>;

pub async fn install_plugin(
    plugin: &str,
    force: bool,
    progress: InstallProgress,
) -> anyhow::Result<Vec<CheckReport>> {
    let mut reports = Vec::new();
    for step in install_steps_for(plugin)? {
        let progress_cb = Arc::clone(&progress);
        let report = browser_setup::run_step(&step, force, move |event| {
            (progress_cb)(event);
        })
        .await?;
        reports.push(report);
    }
    Ok(reports)
}

pub fn install_steps_for(plugin: &str) -> anyhow::Result<Vec<String>> {
    let registry = builtin_registry()?;
    let activation_order = registry.activation_order()?;
    let mut required = BTreeSet::new();
    collect_plugin_and_dependencies(plugin, &registry, &mut required)?;

    let steps = activation_order
        .into_iter()
        .filter(|id| required.contains(id) && has_install_step(id))
        .collect::<Vec<_>>();

    if steps.is_empty() {
        anyhow::bail!("plugin `{plugin}` does not have an install step in this release");
    }

    Ok(steps)
}

fn collect_plugin_and_dependencies(
    plugin: &str,
    registry: &PluginRegistry,
    required: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    let manifest = registry
        .get(plugin)
        .ok_or_else(|| anyhow::anyhow!("unknown plugin `{plugin}`"))?;
    for dependency in &manifest.dependencies {
        collect_plugin_and_dependencies(dependency, registry, required)?;
    }
    required.insert(plugin.to_string());
    Ok(())
}

fn has_install_step(plugin: &str) -> bool {
    matches!(plugin, "node" | "python" | "browser-playwright-cli")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_plugin_install_steps_include_node_dependency_first() {
        assert_eq!(
            install_steps_for("browser-playwright-cli").unwrap(),
            vec!["node".to_string(), "browser-playwright-cli".to_string()]
        );
    }

    #[test]
    fn node_plugin_install_steps_include_only_node() {
        assert_eq!(install_steps_for("node").unwrap(), vec!["node".to_string()]);
    }

    #[test]
    fn python_plugin_install_steps_include_only_python() {
        assert_eq!(
            install_steps_for("python").unwrap(),
            vec!["python".to_string()]
        );
    }

    #[test]
    fn shell_plugin_has_no_install_step() {
        let err = install_steps_for("shell").unwrap_err().to_string();
        assert!(err.contains("does not have an install step"));
    }
}
