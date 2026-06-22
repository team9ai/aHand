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

    pub fn activation_order(&self) -> anyhow::Result<Vec<String>> {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Mark {
            Visiting,
            Done,
        }

        fn visit(
            id: &str,
            registry: &PluginRegistry,
            marks: &mut BTreeMap<String, Mark>,
            order: &mut Vec<String>,
        ) -> anyhow::Result<()> {
            match marks.get(id).copied() {
                Some(Mark::Done) => return Ok(()),
                Some(Mark::Visiting) => anyhow::bail!("plugin dependency cycle includes `{id}`"),
                None => {}
            }

            let manifest = registry
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("plugin `{id}` is not registered"))?;

            marks.insert(id.to_string(), Mark::Visiting);
            for dep in &manifest.dependencies {
                if registry.get(dep).is_none() {
                    anyhow::bail!("plugin `{}` depends on missing plugin `{dep}`", manifest.id);
                }
                visit(dep, registry, marks, order)?;
            }
            marks.insert(id.to_string(), Mark::Done);
            order.push(id.to_string());
            Ok(())
        }

        let mut marks = BTreeMap::new();
        let mut order = Vec::new();
        for id in self.manifests.keys() {
            visit(id, self, &mut marks, &mut order)?;
        }
        Ok(order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(id: &str, deps: &[&str]) -> PluginManifest {
        PluginManifest {
            id: id.to_string(),
            version: "0.1.0".to_string(),
            display_name: id.to_string(),
            dependencies: deps.iter().map(|d| d.to_string()).collect(),
            capabilities: vec![],
            resources: Default::default(),
            help: None,
        }
    }

    #[test]
    fn activation_order_places_dependencies_first() {
        let registry = PluginRegistry::new(vec![
            manifest("browser-playwright-cli", &["shell", "node"]),
            manifest("node", &[]),
            manifest("shell", &[]),
        ])
        .unwrap();

        let order = registry.activation_order().unwrap();
        let browser = order
            .iter()
            .position(|id| id == "browser-playwright-cli")
            .unwrap();
        let shell = order.iter().position(|id| id == "shell").unwrap();
        let node = order.iter().position(|id| id == "node").unwrap();

        assert!(shell < browser);
        assert!(node < browser);
    }

    #[test]
    fn activation_order_rejects_missing_dependency() {
        let registry =
            PluginRegistry::new(vec![manifest("browser-playwright-cli", &["node"])]).unwrap();

        let err = registry.activation_order().unwrap_err().to_string();
        assert!(err.contains("depends on missing plugin `node`"));
    }

    #[test]
    fn activation_order_rejects_cycles() {
        let registry =
            PluginRegistry::new(vec![manifest("a", &["b"]), manifest("b", &["a"])]).unwrap();

        let err = registry.activation_order().unwrap_err().to_string();
        assert!(err.contains("cycle"));
    }
}
