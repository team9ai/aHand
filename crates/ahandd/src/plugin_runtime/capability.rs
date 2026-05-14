use std::collections::BTreeMap;

use super::PluginStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityKind {
    Exec,
    File,
    Browser,
}

impl CapabilityKind {
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Exec => "exec",
            Self::File => "file",
            Self::Browser => "browser-playwright-cli",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Exec => "exec",
            Self::File => "file",
            Self::Browser => "browser",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityRemediation {
    None,
    HostConfiguration { message: String },
    HostEnvironment { message: String },
    InstallPlugin { plugin_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityUnavailable {
    pub capability: CapabilityKind,
    pub plugin_id: String,
    pub status: PluginStatus,
    pub reason: String,
    pub remediation: CapabilityRemediation,
}

impl CapabilityUnavailable {
    pub fn to_protocol_message(&self) -> String {
        match &self.remediation {
            CapabilityRemediation::InstallPlugin { plugin_id } => {
                format!(
                    "{} capability unavailable: plugin {} is {} because {}; install plugin {} through the host plugin installer",
                    self.capability.display_name(),
                    self.plugin_id,
                    plugin_status_word(self.status),
                    self.reason,
                    plugin_id
                )
            }
            CapabilityRemediation::HostConfiguration { message: hint }
            | CapabilityRemediation::HostEnvironment { message: hint } => {
                format!(
                    "{} capability unavailable: {}; {}",
                    self.capability.display_name(),
                    self.reason,
                    hint
                )
            }
            CapabilityRemediation::None => {
                format!(
                    "{} capability unavailable: {}",
                    self.capability.display_name(),
                    self.reason
                )
            }
        }
    }
}

fn plugin_status_word(status: PluginStatus) -> &'static str {
    match status {
        PluginStatus::Installed => "installed",
        PluginStatus::Missing => "missing",
        PluginStatus::Outdated => "outdated",
        PluginStatus::Failed => "failed",
        PluginStatus::Blocked => "blocked",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityState {
    Active,
    Unavailable(CapabilityUnavailable),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityEntry {
    pub capability: CapabilityKind,
    pub owner_plugin_id: String,
    pub state: CapabilityState,
}

impl CapabilityEntry {
    pub fn active(capability: CapabilityKind, owner_plugin_id: impl Into<String>) -> Self {
        Self {
            capability,
            owner_plugin_id: owner_plugin_id.into(),
            state: CapabilityState::Active,
        }
    }

    pub fn unavailable(unavailable: CapabilityUnavailable) -> Self {
        Self {
            capability: unavailable.capability,
            owner_plugin_id: unavailable.plugin_id.clone(),
            state: CapabilityState::Unavailable(unavailable),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityRouter {
    entries: BTreeMap<CapabilityKind, CapabilityEntry>,
}

impl CapabilityRouter {
    pub fn new(entries: Vec<CapabilityEntry>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|entry| (entry.capability, entry))
                .collect(),
        }
    }

    pub fn ensure(&self, capability: CapabilityKind) -> Result<(), CapabilityUnavailable> {
        match self.entries.get(&capability).map(|entry| &entry.state) {
            Some(CapabilityState::Active) => Ok(()),
            Some(CapabilityState::Unavailable(unavailable)) => Err(unavailable.clone()),
            None => Err(CapabilityUnavailable {
                capability,
                plugin_id: capability.wire_name().to_string(),
                status: PluginStatus::Missing,
                reason: "capability is not registered".to_string(),
                remediation: CapabilityRemediation::None,
            }),
        }
    }

    pub fn active_wire_capabilities(&self) -> Vec<&'static str> {
        [
            CapabilityKind::Exec,
            CapabilityKind::File,
            CapabilityKind::Browser,
        ]
        .into_iter()
        .filter(|capability| self.ensure(*capability).is_ok())
        .map(CapabilityKind::wire_name)
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_remediation_renders_host_neutral_message() {
        let unavailable = CapabilityUnavailable {
            capability: CapabilityKind::Browser,
            plugin_id: "browser-playwright-cli".to_string(),
            status: PluginStatus::Blocked,
            reason: "dependency node is missing".to_string(),
            remediation: CapabilityRemediation::InstallPlugin {
                plugin_id: "browser-playwright-cli".to_string(),
            },
        };

        assert_eq!(
            unavailable.to_protocol_message(),
            "browser capability unavailable: plugin browser-playwright-cli is blocked because dependency node is missing; install plugin browser-playwright-cli through the host plugin installer"
        );
    }

    #[test]
    fn builtin_file_disabled_renders_configuration_message() {
        let unavailable = CapabilityUnavailable {
            capability: CapabilityKind::File,
            plugin_id: "file".to_string(),
            status: PluginStatus::Blocked,
            reason: "host configuration disabled file operations".to_string(),
            remediation: CapabilityRemediation::HostConfiguration {
                message: "enable file operations in host configuration".to_string(),
            },
        };

        assert_eq!(
            unavailable.to_protocol_message(),
            "file capability unavailable: host configuration disabled file operations; enable file operations in host configuration"
        );
    }

    #[test]
    fn active_wire_capabilities_use_existing_protocol_names() {
        let router = CapabilityRouter::new(vec![
            CapabilityEntry::active(CapabilityKind::Exec, "shell"),
            CapabilityEntry::active(CapabilityKind::File, "file"),
            CapabilityEntry::active(CapabilityKind::Browser, "browser-playwright-cli"),
        ]);

        assert_eq!(
            router.active_wire_capabilities(),
            vec!["exec", "file", "browser-playwright-cli"]
        );
    }

    #[test]
    fn ensure_returns_unavailable_for_inactive_capability() {
        let unavailable = CapabilityUnavailable {
            capability: CapabilityKind::Exec,
            plugin_id: "shell".to_string(),
            status: PluginStatus::Missing,
            reason: "host shell unavailable".to_string(),
            remediation: CapabilityRemediation::HostEnvironment {
                message: "configure a valid host shell".to_string(),
            },
        };
        let router = CapabilityRouter::new(vec![CapabilityEntry::unavailable(unavailable.clone())]);

        assert_eq!(
            router.ensure(CapabilityKind::Exec).unwrap_err(),
            unavailable
        );
    }
}
