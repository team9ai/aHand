use std::collections::BTreeMap;

use crate::browser::BrowserManager;
use crate::executor::ExecutionTarget;
use crate::file_manager::FileManager;

use super::{
    ActivationConfig, CapabilityKind, CapabilityRemediation, CapabilityRouter,
    CapabilityUnavailable, HostResourceValue, InstalledPluginResource, PluginStatus,
    router_from_plugins,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobProviderKind {
    DefaultExec,
    ManagedRuntime(CapabilityKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobProvider {
    DefaultExec,
    ManagedRuntime {
        capability: CapabilityKind,
        target: ExecutionTarget,
    },
}

#[derive(Debug, Clone)]
pub struct CapabilityProviderRegistry {
    router: CapabilityRouter,
    runtime_targets: BTreeMap<CapabilityKind, ExecutionTarget>,
}

pub async fn build_provider_registry(
    browser_mgr: &BrowserManager,
    file_mgr: &FileManager,
) -> anyhow::Result<CapabilityProviderRegistry> {
    let snapshot = super::get_host_resource().await?;
    let browser_providers = browser_mgr.available_providers();
    Ok(CapabilityProviderRegistry::from_plugins(
        &snapshot.plugins,
        ActivationConfig {
            browser_enabled: browser_mgr.is_enabled(),
            file_enabled: file_mgr.is_enabled(),
            system_browser_available: browser_mgr.has_system_browser(),
            playwright_provider_enabled: browser_mgr.playwright_provider_enabled(),
            cdp_provider_available: browser_providers.contains(&"cdp"),
        },
    ))
}

impl CapabilityProviderRegistry {
    pub fn from_plugins(plugins: &[InstalledPluginResource], config: ActivationConfig) -> Self {
        let router = router_from_plugins(plugins, config);
        let mut runtime_targets = BTreeMap::new();
        if let Some(target) = executable_target(plugins, "node", "node") {
            runtime_targets.insert(CapabilityKind::NodeExec, target);
        }
        if let Some(target) = executable_target(plugins, "python", "python") {
            runtime_targets.insert(CapabilityKind::PythonExec, target);
        }
        Self {
            router,
            runtime_targets,
        }
    }

    pub fn ensure(&self, capability: CapabilityKind) -> Result<(), CapabilityUnavailable> {
        self.router.ensure(capability)
    }

    // Plugin-runtime API surface; consumed by later plugin stages.
    #[allow(dead_code)]
    pub fn active_wire_capabilities(&self) -> Vec<&'static str> {
        self.router.active_wire_capabilities()
    }

    pub fn resolve_job_provider(&self, tool: &str) -> Result<JobProvider, CapabilityUnavailable> {
        match resolve_job_provider_kind(tool) {
            JobProviderKind::DefaultExec => {
                self.ensure(CapabilityKind::Exec)?;
                Ok(JobProvider::DefaultExec)
            }
            JobProviderKind::ManagedRuntime(capability) => {
                self.ensure(capability)?;
                let target = self
                    .runtime_targets
                    .get(&capability)
                    .cloned()
                    .ok_or_else(|| missing_runtime_target(capability))?;
                Ok(JobProvider::ManagedRuntime { capability, target })
            }
        }
    }
}

pub fn resolve_job_provider_kind(tool: &str) -> JobProviderKind {
    match tool {
        "plugin:node" => JobProviderKind::ManagedRuntime(CapabilityKind::NodeExec),
        "plugin:python" => JobProviderKind::ManagedRuntime(CapabilityKind::PythonExec),
        _ => JobProviderKind::DefaultExec,
    }
}

fn executable_target(
    plugins: &[InstalledPluginResource],
    plugin_id: &str,
    resource_name: &str,
) -> Option<ExecutionTarget> {
    let plugin = plugins.iter().find(|plugin| plugin.id == plugin_id)?;
    match plugin.resources.get(resource_name)? {
        HostResourceValue::Executable { path, .. } => Some(ExecutionTarget {
            path: path.clone(),
            leading_args: Vec::new(),
        }),
        _ => None,
    }
}

fn missing_runtime_target(capability: CapabilityKind) -> CapabilityUnavailable {
    let plugin_id = match capability {
        CapabilityKind::NodeExec => "node",
        CapabilityKind::PythonExec => "python",
        _ => capability.wire_name(),
    };
    CapabilityUnavailable {
        capability,
        plugin_id: plugin_id.to_string(),
        status: PluginStatus::Missing,
        reason: format!("{plugin_id} executable resource is missing"),
        remediation: CapabilityRemediation::InstallPlugin {
            plugin_id: plugin_id.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_runtime::CapabilityKind;
    use std::collections::BTreeMap;

    #[test]
    fn explicit_plugin_tokens_select_managed_runtime_providers() {
        assert_eq!(
            resolve_job_provider_kind("plugin:node"),
            JobProviderKind::ManagedRuntime(CapabilityKind::NodeExec)
        );
        assert_eq!(
            resolve_job_provider_kind("plugin:python"),
            JobProviderKind::ManagedRuntime(CapabilityKind::PythonExec)
        );
    }

    #[test]
    fn plain_node_and_python_stay_default_exec_provider() {
        assert_eq!(
            resolve_job_provider_kind("node"),
            JobProviderKind::DefaultExec
        );
        assert_eq!(
            resolve_job_provider_kind("python"),
            JobProviderKind::DefaultExec
        );
    }

    #[test]
    fn managed_runtime_provider_uses_exported_executable_path() {
        let mut node_resources = BTreeMap::new();
        node_resources.insert(
            "node".to_string(),
            HostResourceValue::Executable {
                name: "node".to_string(),
                path: "/tmp/ahand/node/bin/node".to_string(),
                version: Some("v24.13.0".to_string()),
            },
        );
        let plugins = vec![InstalledPluginResource {
            id: "node".to_string(),
            version: "0.1.0".to_string(),
            status: PluginStatus::Installed,
            dependencies: Vec::new(),
            capabilities: Vec::new(),
            resources: node_resources,
            help_prompt: None,
        }];
        let registry = CapabilityProviderRegistry::from_plugins(
            &plugins,
            ActivationConfig {
                browser_enabled: false,
                file_enabled: false,
                system_browser_available: false,
                playwright_provider_enabled: false,
                cdp_provider_available: false,
            },
        );

        let provider = registry.resolve_job_provider("plugin:node").unwrap();

        assert_eq!(
            provider,
            JobProvider::ManagedRuntime {
                capability: CapabilityKind::NodeExec,
                target: ExecutionTarget {
                    path: "/tmp/ahand/node/bin/node".to_string(),
                    leading_args: Vec::new(),
                },
            }
        );
    }
}
