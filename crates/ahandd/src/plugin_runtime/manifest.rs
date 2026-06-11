use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub version: String,
    pub display_name: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub resources: ResourceManifest,
    #[serde(default)]
    pub help: Option<HelpManifest>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceManifest {
    #[serde(default)]
    pub executables: BTreeMap<String, ExecutableResourceManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutableResourceManifest {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelpManifest {
    pub prompt: String,
}

impl PluginManifest {
    pub fn parse(input: &str) -> anyhow::Result<Self> {
        let manifest: Self = toml::from_str(input)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.id.trim().is_empty() {
            anyhow::bail!("plugin id cannot be empty");
        }
        if self.version.trim().is_empty() {
            anyhow::bail!("plugin `{}` version cannot be empty", self.id);
        }
        if self.display_name.trim().is_empty() {
            anyhow::bail!("plugin `{}` display_name cannot be empty", self.id);
        }
        for dep in &self.dependencies {
            if dep.trim().is_empty() {
                anyhow::bail!("plugin `{}` has an empty dependency id", self.id);
            }
            if dep == &self.id {
                anyhow::bail!("plugin `{}` cannot depend on itself", self.id);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_browser_playwright_manifest() {
        let manifest = PluginManifest::parse(
            r#"
id = "browser-playwright-cli"
version = "0.1.0"
display_name = "Browser Playwright CLI"
dependencies = ["shell", "node"]
capabilities = ["browser"]

[resources.executables.playwrightCli]
name = "playwright-cli"
path = "plugins/browser-playwright-cli/bin/playwright-cli"

[help]
prompt = "Provides browser automation through playwright-cli."
"#,
        )
        .unwrap();

        assert_eq!(manifest.id, "browser-playwright-cli");
        assert_eq!(manifest.dependencies, vec!["shell", "node"]);
        assert_eq!(manifest.capabilities, vec!["browser"]);
        assert_eq!(
            manifest.help.as_ref().unwrap().prompt,
            "Provides browser automation through playwright-cli."
        );
        assert_eq!(
            manifest.resources.executables["playwrightCli"].path,
            "plugins/browser-playwright-cli/bin/playwright-cli"
        );
    }

    #[test]
    fn rejects_empty_plugin_id() {
        let err = PluginManifest::parse(
            r#"
id = ""
version = "0.1.0"
display_name = "Invalid"
"#,
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("plugin id cannot be empty"));
    }
}
