use crate::browser::BrowserManager;
use crate::file_manager::FileManager;

use super::{
    CapabilityEntry, CapabilityKind, CapabilityRemediation, CapabilityRouter,
    CapabilityUnavailable, InstalledPluginResource, PluginStatus,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivationConfig {
    pub browser_enabled: bool,
    pub file_enabled: bool,
    pub system_browser_available: bool,
}

pub async fn build_router(
    browser_mgr: &BrowserManager,
    file_mgr: &FileManager,
) -> anyhow::Result<CapabilityRouter> {
    let snapshot = super::get_host_resource().await?;
    Ok(router_from_plugins(
        &snapshot.plugins,
        ActivationConfig {
            browser_enabled: browser_mgr.is_enabled(),
            file_enabled: file_mgr.is_enabled(),
            system_browser_available: browser_mgr.has_system_browser(),
        },
    ))
}

pub fn router_from_plugins(
    plugins: &[InstalledPluginResource],
    config: ActivationConfig,
) -> CapabilityRouter {
    CapabilityRouter::new(vec![
        exec_entry(plugins),
        file_entry(plugins, config),
        browser_entry(plugins, config),
    ])
}

fn exec_entry(plugins: &[InstalledPluginResource]) -> CapabilityEntry {
    match plugin_status(plugins, "shell") {
        Some(PluginStatus::Installed) => CapabilityEntry::active(CapabilityKind::Exec, "shell"),
        Some(status) => CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::Exec,
            plugin_id: "shell".to_string(),
            status,
            reason: "host shell unavailable".to_string(),
            remediation: CapabilityRemediation::HostEnvironment {
                message: "configure a valid host shell".to_string(),
            },
        }),
        None => CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::Exec,
            plugin_id: "shell".to_string(),
            status: PluginStatus::Missing,
            reason: "shell plugin is not registered".to_string(),
            remediation: CapabilityRemediation::HostEnvironment {
                message: "configure a valid host shell".to_string(),
            },
        }),
    }
}

fn file_entry(plugins: &[InstalledPluginResource], config: ActivationConfig) -> CapabilityEntry {
    if !config.file_enabled {
        return CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::File,
            plugin_id: "file".to_string(),
            status: PluginStatus::Blocked,
            reason: "host configuration disabled file operations".to_string(),
            remediation: CapabilityRemediation::HostConfiguration {
                message: "enable file operations in host configuration".to_string(),
            },
        });
    }

    match plugin_status(plugins, "file") {
        Some(PluginStatus::Installed) => CapabilityEntry::active(CapabilityKind::File, "file"),
        Some(status) => CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::File,
            plugin_id: "file".to_string(),
            status,
            reason: format!("file plugin is {}", status_word(status)),
            remediation: CapabilityRemediation::HostEnvironment {
                message: "file capability is not available in this host".to_string(),
            },
        }),
        None => CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::File,
            plugin_id: "file".to_string(),
            status: PluginStatus::Missing,
            reason: "file plugin is not registered".to_string(),
            remediation: CapabilityRemediation::HostEnvironment {
                message: "file capability is not available in this host".to_string(),
            },
        }),
    }
}

fn browser_entry(plugins: &[InstalledPluginResource], config: ActivationConfig) -> CapabilityEntry {
    if !config.browser_enabled {
        return CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::Browser,
            plugin_id: "browser-playwright-cli".to_string(),
            status: PluginStatus::Blocked,
            reason: "host configuration disabled browser capabilities".to_string(),
            remediation: CapabilityRemediation::HostConfiguration {
                message: "enable browser capabilities in host configuration".to_string(),
            },
        });
    }

    if let Some(reason) = first_missing_dependency(plugins, "browser-playwright-cli") {
        return CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::Browser,
            plugin_id: "browser-playwright-cli".to_string(),
            status: PluginStatus::Blocked,
            reason,
            remediation: CapabilityRemediation::InstallPlugin {
                plugin_id: "browser-playwright-cli".to_string(),
            },
        });
    }

    match plugin_status(plugins, "browser-playwright-cli") {
        Some(PluginStatus::Installed) if config.system_browser_available => {
            CapabilityEntry::active(CapabilityKind::Browser, "browser-playwright-cli")
        }
        Some(PluginStatus::Installed) => CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::Browser,
            plugin_id: "browser-playwright-cli".to_string(),
            status: PluginStatus::Blocked,
            reason: "no supported system browser is available".to_string(),
            remediation: CapabilityRemediation::HostEnvironment {
                message: "install or configure a supported system browser".to_string(),
            },
        }),
        Some(status) => CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::Browser,
            plugin_id: "browser-playwright-cli".to_string(),
            status,
            reason: format!("browser-playwright-cli plugin is {}", status_word(status)),
            remediation: CapabilityRemediation::InstallPlugin {
                plugin_id: "browser-playwright-cli".to_string(),
            },
        }),
        None => CapabilityEntry::unavailable(CapabilityUnavailable {
            capability: CapabilityKind::Browser,
            plugin_id: "browser-playwright-cli".to_string(),
            status: PluginStatus::Missing,
            reason: "browser-playwright-cli plugin is not registered".to_string(),
            remediation: CapabilityRemediation::InstallPlugin {
                plugin_id: "browser-playwright-cli".to_string(),
            },
        }),
    }
}

fn first_missing_dependency(plugins: &[InstalledPluginResource], plugin_id: &str) -> Option<String> {
    let plugin = plugins.iter().find(|plugin| plugin.id == plugin_id)?;
    plugin.dependencies.iter().find_map(|dependency| {
        let status = plugin_status(plugins, dependency).unwrap_or(PluginStatus::Missing);
        if status == PluginStatus::Installed {
            None
        } else {
            Some(format!("dependency {} is {}", dependency, status_word(status)))
        }
    })
}

fn plugin_status(plugins: &[InstalledPluginResource], plugin_id: &str) -> Option<PluginStatus> {
    plugins
        .iter()
        .find(|plugin| plugin.id == plugin_id)
        .map(|plugin| plugin.status)
}

fn status_word(status: PluginStatus) -> &'static str {
    match status {
        PluginStatus::Installed => "installed",
        PluginStatus::Missing => "missing",
        PluginStatus::Outdated => "outdated",
        PluginStatus::Failed => "failed",
        PluginStatus::Blocked => "blocked",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_runtime::{InstalledPluginResource, PluginStatus};
    use std::collections::BTreeMap;

    fn plugin(id: &str, status: PluginStatus, dependencies: &[&str]) -> InstalledPluginResource {
        InstalledPluginResource {
            id: id.to_string(),
            version: "0.1.0".to_string(),
            status,
            dependencies: dependencies.iter().map(|dep| dep.to_string()).collect(),
            capabilities: Vec::new(),
            resources: BTreeMap::new(),
            help_prompt: None,
        }
    }

    fn base_plugins() -> Vec<InstalledPluginResource> {
        vec![
            plugin("shell", PluginStatus::Installed, &[]),
            plugin("file", PluginStatus::Installed, &[]),
            plugin("node", PluginStatus::Installed, &[]),
            plugin(
                "browser-playwright-cli",
                PluginStatus::Installed,
                &["shell", "node"],
            ),
        ]
    }

    #[test]
    fn file_disabled_by_host_config_is_not_installable() {
        let router = router_from_plugins(
            &base_plugins(),
            ActivationConfig {
                browser_enabled: true,
                file_enabled: false,
                system_browser_available: true,
            },
        );

        let err = router.ensure(CapabilityKind::File).unwrap_err();
        assert_eq!(err.plugin_id, "file");
        assert_eq!(
            err.remediation,
            CapabilityRemediation::HostConfiguration {
                message: "enable file operations in host configuration".to_string()
            }
        );
    }

    #[test]
    fn browser_missing_node_recommends_installing_browser_plugin() {
        let mut plugins = base_plugins();
        plugins[2].status = PluginStatus::Missing;
        plugins[3].status = PluginStatus::Blocked;

        let router = router_from_plugins(
            &plugins,
            ActivationConfig {
                browser_enabled: true,
                file_enabled: true,
                system_browser_available: true,
            },
        );

        let err = router.ensure(CapabilityKind::Browser).unwrap_err();
        assert_eq!(err.plugin_id, "browser-playwright-cli");
        assert_eq!(err.reason, "dependency node is missing");
        assert_eq!(
            err.remediation,
            CapabilityRemediation::InstallPlugin {
                plugin_id: "browser-playwright-cli".to_string()
            }
        );
    }

    #[test]
    fn browser_disabled_by_host_config_does_not_suggest_install() {
        let router = router_from_plugins(
            &base_plugins(),
            ActivationConfig {
                browser_enabled: false,
                file_enabled: true,
                system_browser_available: true,
            },
        );

        let err = router.ensure(CapabilityKind::Browser).unwrap_err();
        assert_eq!(
            err.remediation,
            CapabilityRemediation::HostConfiguration {
                message: "enable browser capabilities in host configuration".to_string()
            }
        );
    }

    #[test]
    fn missing_system_browser_is_host_environment_error() {
        let router = router_from_plugins(
            &base_plugins(),
            ActivationConfig {
                browser_enabled: true,
                file_enabled: true,
                system_browser_available: false,
            },
        );

        let err = router.ensure(CapabilityKind::Browser).unwrap_err();
        assert_eq!(
            err.remediation,
            CapabilityRemediation::HostEnvironment {
                message: "install or configure a supported system browser".to_string()
            }
        );
    }
}
