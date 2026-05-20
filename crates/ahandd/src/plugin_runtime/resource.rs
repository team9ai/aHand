use std::collections::BTreeMap;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginStatus {
    Installed,
    Missing,
    Outdated,
    Failed,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostResourceSnapshot {
    pub runtime_version: String,
    pub platform: String,
    pub arch: String,
    pub plugins: Vec<InstalledPluginResource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPluginResource {
    pub id: String,
    pub version: String,
    pub status: PluginStatus,
    pub dependencies: Vec<String>,
    pub capabilities: Vec<String>,
    pub resources: BTreeMap<String, HostResourceValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<RuntimePackage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help_prompt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePackage {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostResourceValue {
    Executable {
        name: String,
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        version: Option<String>,
    },
    Directory {
        name: String,
        path: String,
    },
    Env {
        name: String,
        value: String,
    },
    Config {
        name: String,
        value: serde_json::Value,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn host_resource_snapshot_serializes_camel_case() {
        let mut resources = BTreeMap::new();
        resources.insert(
            "node".to_string(),
            HostResourceValue::Executable {
                name: "node".to_string(),
                path: "/tmp/node/bin/node".to_string(),
                version: Some("v24.14.0".to_string()),
            },
        );

        let snapshot = HostResourceSnapshot {
            runtime_version: "0.1.0".to_string(),
            platform: "darwin".to_string(),
            arch: "arm64".to_string(),
            plugins: vec![InstalledPluginResource {
                id: "node".to_string(),
                version: "0.1.0".to_string(),
                status: PluginStatus::Installed,
                dependencies: vec![],
                capabilities: vec![],
                resources,
                packages: vec![RuntimePackage {
                    name: "typescript".to_string(),
                    version: Some("5.9.3".to_string()),
                }],
                help_prompt: Some("Use managed Node.js.".to_string()),
            }],
        };

        let actual = serde_json::to_value(snapshot).unwrap();
        assert_eq!(
            actual,
            json!({
                "runtimeVersion": "0.1.0",
                "platform": "darwin",
                "arch": "arm64",
                "plugins": [{
                    "id": "node",
                    "version": "0.1.0",
                    "status": "installed",
                    "dependencies": [],
                    "capabilities": [],
                    "resources": {
                        "node": {
                            "kind": "executable",
                            "name": "node",
                            "path": "/tmp/node/bin/node",
                            "version": "v24.14.0"
                        }
                    },
                    "packages": [{
                        "name": "typescript",
                        "version": "5.9.3"
                    }],
                    "helpPrompt": "Use managed Node.js."
                }]
            })
        );
    }
}
