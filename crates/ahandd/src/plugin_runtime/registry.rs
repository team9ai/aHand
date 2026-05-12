use std::collections::BTreeMap;

use super::manifest::PluginManifest;

#[derive(Debug, Clone)]
pub struct PluginRegistry {
    manifests: BTreeMap<String, PluginManifest>,
}

impl PluginRegistry {
    pub fn new(manifests: Vec<PluginManifest>) -> anyhow::Result<Self> {
        let mut by_id = BTreeMap::new();
        for manifest in manifests {
            manifest.validate()?;
            if by_id.insert(manifest.id.clone(), manifest).is_some() {
                anyhow::bail!("duplicate plugin id in registry");
            }
        }
        Ok(Self { manifests: by_id })
    }

    pub fn get(&self, id: &str) -> Option<&PluginManifest> {
        self.manifests.get(id)
    }

    pub fn manifests(&self) -> impl Iterator<Item = &PluginManifest> {
        self.manifests.values()
    }
}
