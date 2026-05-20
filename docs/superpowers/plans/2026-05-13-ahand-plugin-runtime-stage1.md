# AHand Plugin Runtime Stage 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the first plugin runtime foundation for aHand, expose `getHostResource`, and migrate Node/playwright setup behind first-party plugin metadata while preserving existing browser setup commands.

**Architecture:** Add a static first-party plugin registry under `crates/ahandd/src/plugin_runtime/` with TOML manifest parsing, dependency ordering, runtime directory helpers, and host resource serialization. Reuse the existing `browser_setup` installer code as plugin lifecycle adapters, moving managed Node/playwright paths into `~/.cache/ahand-runtimes/ahand-primary-runtime`. Expose host resources through `ahandd plugin host-resource --json` and the local admin API without changing the protobuf wire protocol.

**Tech Stack:** Rust 2024, tokio, clap, serde, serde_json, toml, existing `browser_setup` modules, ahandctl warp admin server, Solid admin API client.

**Spec:** `docs/superpowers/specs/2026-05-12-ahand-plugin-runtime-design.md`

---

## Scope Check

This plan covers Stage 1 from the spec only:

- plugin manifest types.
- static first-party plugin registry.
- dependency graph ordering and cycle rejection.
- host resource snapshot model.
- `node` and `browser-playwright-cli` inspection/install paths.
- compatibility wrappers for `browser-init` and `browser-doctor`.
- local/admin JSON `getHostResource` surface.

The Stage 2 capability activation work is intentionally excluded from this plan. `JobRequest`, `BrowserRequest`, and `FileRequest` continue through their current handlers.

## File Structure

### Created

| File | Responsibility |
|------|----------------|
| `crates/ahandd/src/plugin_runtime/mod.rs` | Public module entry point and re-exports for plugin runtime types and helpers. |
| `crates/ahandd/src/plugin_runtime/manifest.rs` | TOML manifest data structures and parsing/validation. |
| `crates/ahandd/src/plugin_runtime/resource.rs` | Plugin status, resource values, host resource snapshot structs, and serialization tests. |
| `crates/ahandd/src/plugin_runtime/runtime_dir.rs` | `~/.cache/ahand-runtimes/ahand-primary-runtime` path helpers. |
| `crates/ahandd/src/plugin_runtime/registry.rs` | Static registry, dependency graph ordering, dependency status checks, and cycle detection. |
| `crates/ahandd/src/plugin_runtime/builtin.rs` | Built-in TOML manifests for `shell`, `node`, `python`, `file`, and `browser-playwright-cli`. |
| `crates/ahandd/src/plugin_runtime/host_resource.rs` | Async aggregation of installed plugin resources from manifests and existing inspectors. |

### Modified

| File | Change |
|------|--------|
| `crates/ahandd/src/lib.rs` | Export `plugin_runtime`. |
| `crates/ahandd/src/main.rs` | Add `plugin` subcommands for doctor/install/repair/host-resource and route them through `plugin_runtime`. |
| `crates/ahandd/src/browser_setup/node.rs` | Replace `~/.ahand/node` paths with runtime directory helper paths. |
| `crates/ahandd/src/browser_setup/playwright.rs` | Resolve `npm` and `playwright-cli` through the node runtime directory helper. |
| `crates/ahandd/src/browser_setup/mod.rs` | Add plugin-name aliases for inspect/run paths while keeping `node`, `playwright`, and `browser` compatibility. |
| `crates/ahandd/src/browser.rs` | Default `playwright-cli` path comes from the plugin runtime when `browser.binary_path` is unset. |
| `crates/ahandd/src/cli/browser_init.rs` | Keep existing command output but call plugin ids internally. |
| `crates/ahandd/src/cli/browser_doctor.rs` | Keep existing browser-focused doctor behavior using compatibility aliases. |
| `crates/ahandctl/Cargo.toml` | Add dependency on the `ahandd` library target for host resource JSON. |
| `crates/ahandctl/src/admin.rs` | Add `GET /api/host-resource` route. |
| `apps/admin/src/lib/api.ts` | Add host resource TypeScript types and `api.getHostResource()`. |

---

### Task 1: Add Plugin Manifest And Resource Types

**Files:**
- Create: `crates/ahandd/src/plugin_runtime/mod.rs`
- Create: `crates/ahandd/src/plugin_runtime/manifest.rs`
- Create: `crates/ahandd/src/plugin_runtime/resource.rs`
- Modify: `crates/ahandd/src/lib.rs`
- Test: module tests inside `manifest.rs` and `resource.rs`

- [ ] **Step 1: Write manifest parsing tests**

Add this test module to the new file `crates/ahandd/src/plugin_runtime/manifest.rs` before writing the implementation:

```rust
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
```

- [ ] **Step 2: Run manifest tests to verify they fail**

Run: `cargo test -p ahandd plugin_runtime::manifest`

Expected: compile failure because `PluginManifest` and `plugin_runtime` do not exist yet.

- [ ] **Step 3: Create the plugin runtime module entry point**

Create `crates/ahandd/src/plugin_runtime/mod.rs`:

```rust
pub mod builtin;
pub mod host_resource;
pub mod manifest;
pub mod registry;
pub mod resource;
pub mod runtime_dir;

pub use host_resource::get_host_resource;
pub use manifest::{ExecutableResourceManifest, HelpManifest, PluginManifest, ResourceManifest};
pub use registry::PluginRegistry;
pub use resource::{
    HostResourceSnapshot, HostResourceValue, InstalledPluginResource, PluginStatus,
};
pub use runtime_dir::RuntimeDirs;
```

Modify `crates/ahandd/src/lib.rs`:

```rust
pub mod ahand_client;
pub mod approval;
pub mod browser;
pub mod browser_setup;
pub mod config;
pub mod device_identity;
pub mod executor;
pub mod file_manager;
pub mod outbox;
pub mod plugin_runtime;
pub mod registry;
pub mod session;
pub mod store;
pub mod updater;
```

Keep the existing `mod public_api;` and public re-exports below this block unchanged.

- [ ] **Step 4: Implement manifest parsing**

Create `crates/ahandd/src/plugin_runtime/manifest.rs` with:

```rust
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
```

Keep the tests from Step 1 at the bottom of the file.

- [ ] **Step 5: Write resource serialization tests**

Create `crates/ahandd/src/plugin_runtime/resource.rs` with only this test module first:

```rust
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
                    "helpPrompt": "Use managed Node.js."
                }]
            })
        );
    }
}
```

- [ ] **Step 6: Run resource tests to verify they fail**

Run: `cargo test -p ahandd plugin_runtime::resource`

Expected: compile failure because resource structs do not exist yet.

- [ ] **Step 7: Implement resource types**

Put this implementation above the tests in `crates/ahandd/src/plugin_runtime/resource.rs`:

```rust
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help_prompt: Option<String>,
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
```

- [ ] **Step 8: Verify Task 1**

Run: `cargo test -p ahandd plugin_runtime::manifest plugin_runtime::resource`

Expected: all manifest and resource tests pass.

- [ ] **Step 9: Commit Task 1**

```bash
git add crates/ahandd/src/lib.rs crates/ahandd/src/plugin_runtime
git commit -m "feat(ahandd): add plugin manifest resource types"
```

---

### Task 2: Add Runtime Directory Helpers And Built-In Manifests

**Files:**
- Create: `crates/ahandd/src/plugin_runtime/runtime_dir.rs`
- Create: `crates/ahandd/src/plugin_runtime/builtin.rs`
- Test: module tests inside both files

- [ ] **Step 1: Write runtime directory tests**

Create `crates/ahandd/src/plugin_runtime/runtime_dir.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_dirs_use_cache_root() {
        let root = std::path::PathBuf::from("/tmp/cache/ahand-primary-runtime");
        let dirs = RuntimeDirs::from_root(root.clone());

        assert_eq!(dirs.root, root);
        assert_eq!(
            dirs.node_dir(),
            std::path::PathBuf::from("/tmp/cache/ahand-primary-runtime/dependencies/node")
        );
        assert_eq!(
            dirs.playwright_cli_bin(),
            std::path::PathBuf::from(
                "/tmp/cache/ahand-primary-runtime/dependencies/node/bin/playwright-cli"
            )
        );
    }
}
```

- [ ] **Step 2: Run runtime directory test to verify it fails**

Run: `cargo test -p ahandd plugin_runtime::runtime_dir`

Expected: compile failure because `RuntimeDirs` is not implemented.

- [ ] **Step 3: Implement runtime directory helper**

Add this implementation above the tests:

```rust
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDirs {
    pub root: PathBuf,
}

impl RuntimeDirs {
    pub fn new() -> anyhow::Result<Self> {
        let cache = dirs::cache_dir()
            .ok_or_else(|| anyhow::anyhow!("cannot determine user cache directory"))?;
        Ok(Self::from_root(
            cache
                .join("ahand-runtimes")
                .join("ahand-primary-runtime"),
        ))
    }

    pub fn from_root(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn runtime_json(&self) -> PathBuf {
        self.root.join("runtime.json")
    }

    pub fn plugins_dir(&self) -> PathBuf {
        self.root.join("plugins")
    }

    pub fn dependencies_dir(&self) -> PathBuf {
        self.root.join("dependencies")
    }

    pub fn node_dir(&self) -> PathBuf {
        self.dependencies_dir().join("node")
    }

    pub fn node_bin(&self) -> PathBuf {
        self.node_dir().join("bin").join(exe_name("node"))
    }

    pub fn npm_bin(&self) -> PathBuf {
        self.node_dir().join("bin").join(exe_name("npm"))
    }

    pub fn playwright_cli_bin(&self) -> PathBuf {
        self.node_dir().join("bin").join(exe_name("playwright-cli"))
    }

    pub fn python_dir(&self) -> PathBuf {
        self.dependencies_dir().join("python")
    }

    pub fn python_bin(&self) -> PathBuf {
        self.python_dir().join("bin").join(exe_name("python3"))
    }
}

fn exe_name(name: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}
```

- [ ] **Step 4: Write built-in manifest tests**

Create `crates/ahandd/src/plugin_runtime/builtin.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_manifests_parse_and_include_expected_ids() {
        let registry = builtin_registry().unwrap();
        let ids: Vec<_> = registry.manifests().map(|m| m.id.as_str()).collect();

        assert_eq!(
            ids,
            vec!["browser-playwright-cli", "file", "node", "python", "shell"]
        );
    }

    #[test]
    fn browser_playwright_declares_shell_and_node_dependencies() {
        let registry = builtin_registry().unwrap();
        let manifest = registry.get("browser-playwright-cli").unwrap();

        assert_eq!(manifest.dependencies, vec!["shell", "node"]);
        assert_eq!(manifest.capabilities, vec!["browser"]);
    }
}
```

- [ ] **Step 5: Run built-in tests to verify they fail**

Run: `cargo test -p ahandd plugin_runtime::builtin`

Expected: compile failure because `builtin_registry` and `PluginRegistry` do not exist yet.

- [ ] **Step 6: Add a minimal registry container needed by built-ins**

Create `crates/ahandd/src/plugin_runtime/registry.rs`:

```rust
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
```

- [ ] **Step 7: Implement built-in manifests**

Add this implementation above the tests in `builtin.rs`:

```rust
use super::manifest::PluginManifest;
use super::registry::PluginRegistry;

const SHELL: &str = r#"
id = "shell"
version = "0.1.0"
display_name = "Shell"
dependencies = []
capabilities = ["shell"]

[resources.executables.shell]
name = "shell"
path = "$SHELL"

[help]
prompt = "Provides local command execution, PTY sessions, stdin forwarding, terminal resize, cwd/env propagation, and stdout/stderr streaming."
"#;

const NODE: &str = r#"
id = "node"
version = "0.1.0"
display_name = "Node.js Runtime"
dependencies = []
capabilities = []

[resources.executables.node]
name = "node"
path = "dependencies/node/bin/node"

[resources.executables.npm]
name = "npm"
path = "dependencies/node/bin/npm"

[help]
prompt = "Use the managed Node.js runtime for JavaScript-based local tools. Prefer this path over system node when a plugin depends on node."
"#;

const PYTHON: &str = r#"
id = "python"
version = "0.1.0"
display_name = "Python Runtime"
dependencies = []
capabilities = []

[resources.executables.python]
name = "python"
path = "dependencies/python/bin/python3"

[help]
prompt = "Use the managed Python runtime for Python-based local tools and scripts."
"#;

const FILE: &str = r#"
id = "file"
version = "0.1.0"
display_name = "File Operations"
dependencies = []
capabilities = ["file"]

[help]
prompt = "Provides file list, read, write, edit, delete, stat, glob, mkdir, copy, move, symlink, and chmod operations under daemon file policy."
"#;

const BROWSER_PLAYWRIGHT_CLI: &str = r#"
id = "browser-playwright-cli"
version = "0.1.0"
display_name = "Browser Playwright CLI"
dependencies = ["shell", "node"]
capabilities = ["browser"]

[resources.executables.playwrightCli]
name = "playwright-cli"
path = "dependencies/node/bin/playwright-cli"

[help]
prompt = "Provides browser automation through playwright-cli. Use for browser open, click, fill, snapshot, screenshot, PDF, download, and close actions."
"#;

pub fn builtin_registry() -> anyhow::Result<PluginRegistry> {
    let manifests = [SHELL, NODE, PYTHON, FILE, BROWSER_PLAYWRIGHT_CLI]
        .into_iter()
        .map(PluginManifest::parse)
        .collect::<anyhow::Result<Vec<_>>>()?;
    PluginRegistry::new(manifests)
}
```

- [ ] **Step 8: Verify Task 2**

Run: `cargo test -p ahandd plugin_runtime::runtime_dir plugin_runtime::builtin`

Expected: all runtime directory and built-in manifest tests pass.

- [ ] **Step 9: Commit Task 2**

```bash
git add crates/ahandd/src/plugin_runtime
git commit -m "feat(ahandd): add builtin plugin runtime metadata"
```

---

### Task 3: Add Dependency Graph Ordering And Blocking Status

**Files:**
- Modify: `crates/ahandd/src/plugin_runtime/registry.rs`
- Test: module tests inside `registry.rs`

- [ ] **Step 1: Write dependency graph tests**

Append these tests to `registry.rs`:

```rust
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
        let browser = order.iter().position(|id| id == "browser-playwright-cli").unwrap();
        let shell = order.iter().position(|id| id == "shell").unwrap();
        let node = order.iter().position(|id| id == "node").unwrap();

        assert!(shell < browser);
        assert!(node < browser);
    }

    #[test]
    fn activation_order_rejects_missing_dependency() {
        let registry = PluginRegistry::new(vec![manifest("browser-playwright-cli", &["node"])])
            .unwrap();

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
```

- [ ] **Step 2: Run dependency tests to verify they fail**

Run: `cargo test -p ahandd plugin_runtime::registry`

Expected: compile failure because `activation_order` does not exist.

- [ ] **Step 3: Implement dependency graph ordering**

Add this below the existing `manifests()` method in `registry.rs`:

```rust
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
```

- [ ] **Step 4: Verify Task 3**

Run: `cargo test -p ahandd plugin_runtime::registry`

Expected: dependency ordering, missing dependency, and cycle tests pass.

- [ ] **Step 5: Commit Task 3**

```bash
git add crates/ahandd/src/plugin_runtime/registry.rs
git commit -m "feat(ahandd): order plugin dependencies"
```

---

### Task 4: Move Node And Playwright Paths Into Runtime Directory

**Files:**
- Modify: `crates/ahandd/src/browser_setup/node.rs`
- Modify: `crates/ahandd/src/browser_setup/playwright.rs`
- Modify: `crates/ahandd/src/browser.rs`
- Test: existing module tests plus small path tests

- [ ] **Step 1: Write path expectation tests for Node dirs**

In `crates/ahandd/src/browser_setup/node.rs`, add this test inside the existing test module:

```rust
#[test]
fn dirs_use_plugin_runtime_node_directory() {
    let root = PathBuf::from("/tmp/ahand-primary-runtime");
    let dirs = Dirs::from_runtime_root(root);

    assert_eq!(
        dirs.node,
        PathBuf::from("/tmp/ahand-primary-runtime/dependencies/node")
    );
    assert_eq!(
        dirs.local_node_bin(),
        PathBuf::from("/tmp/ahand-primary-runtime/dependencies/node/bin/node")
    );
}
```

- [ ] **Step 2: Run Node path test to verify it fails**

Run: `cargo test -p ahandd browser_setup::node::tests::dirs_use_plugin_runtime_node_directory`

Expected: compile failure because `Dirs::from_runtime_root` and `local_node_bin` do not exist.

- [ ] **Step 3: Refactor Node dirs to use `RuntimeDirs`**

Replace the `Dirs` struct and `local_node_bin()` helper in `node.rs` with:

```rust
pub struct Dirs {
    pub ahand: PathBuf,
    pub node: PathBuf,
    runtime: crate::plugin_runtime::RuntimeDirs,
}

impl Dirs {
    pub fn new() -> Result<Self> {
        let runtime = crate::plugin_runtime::RuntimeDirs::new()?;
        Ok(Self::from_runtime(runtime))
    }

    pub fn from_runtime_root(root: PathBuf) -> Self {
        Self::from_runtime(crate::plugin_runtime::RuntimeDirs::from_root(root))
    }

    fn from_runtime(runtime: crate::plugin_runtime::RuntimeDirs) -> Self {
        let ahand = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".ahand");
        let node = runtime.node_dir();
        Self {
            ahand,
            node,
            runtime,
        }
    }

    pub fn local_node_bin(&self) -> PathBuf {
        self.runtime.node_bin()
    }

    pub fn npm_bin(&self) -> PathBuf {
        self.runtime.npm_bin()
    }

    pub fn playwright_cli_bin(&self) -> PathBuf {
        self.runtime.playwright_cli_bin()
    }
}

fn local_node_bin() -> Result<PathBuf> {
    Ok(Dirs::new()?.local_node_bin())
}
```

In `ensure()`, replace:

```rust
let local_node = dirs.node.join("bin").join("node");
```

with:

```rust
let local_node = dirs.local_node_bin();
```

- [ ] **Step 4: Refactor playwright paths to use Node dirs methods**

In `crates/ahandd/src/browser_setup/playwright.rs`, replace `cli_path()` with:

```rust
pub fn cli_path() -> Result<PathBuf> {
    let dirs = Dirs::new()?;
    Ok(dirs.playwright_cli_bin())
}
```

In `ensure()`, replace:

```rust
let npm = dirs.node.join("bin").join("npm");
```

with:

```rust
let npm = dirs.npm_bin();
```

Keep `let prefix = dirs.node.to_string_lossy().to_string();` because npm still installs into the managed Node prefix.

- [ ] **Step 5: Make BrowserManager default to plugin runtime path**

In `crates/ahandd/src/browser.rs`, replace the default branch of `binary_path()` with:

```rust
None => crate::plugin_runtime::RuntimeDirs::new()
    .map(|dirs| dirs.playwright_cli_bin())
    .unwrap_or_else(|_| PathBuf::from("playwright-cli")),
```

Leave `Some(path) => PathBuf::from(path)` unchanged so explicit config still wins.

- [ ] **Step 6: Verify Task 4**

Run:

```bash
cargo test -p ahandd browser_setup::node browser_setup::playwright browser_setup::types
cargo test -p ahandd --test browser_doctor
```

Expected: all listed tests pass. The browser doctor test may exit 0 or 1, but the test itself must pass.

- [ ] **Step 7: Commit Task 4**

```bash
git add crates/ahandd/src/browser_setup/node.rs crates/ahandd/src/browser_setup/playwright.rs crates/ahandd/src/browser.rs
git commit -m "feat(ahandd): use managed runtime paths for browser setup"
```

---

### Task 5: Aggregate `getHostResource`

**Files:**
- Create: `crates/ahandd/src/plugin_runtime/host_resource.rs`
- Modify: `crates/ahandd/src/plugin_runtime/mod.rs`
- Test: module tests inside `host_resource.rs`

- [ ] **Step 1: Write host resource aggregation tests**

Create `crates/ahandd/src/plugin_runtime/host_resource.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn snapshot_contains_first_party_plugins() {
        let snapshot = get_host_resource().await.unwrap();
        let ids: Vec<_> = snapshot.plugins.iter().map(|p| p.id.as_str()).collect();

        assert_eq!(
            ids,
            vec!["browser-playwright-cli", "file", "node", "python", "shell"]
        );
    }

    #[tokio::test]
    async fn browser_plugin_reports_shell_and_node_dependencies() {
        let snapshot = get_host_resource().await.unwrap();
        let browser = snapshot
            .plugins
            .iter()
            .find(|plugin| plugin.id == "browser-playwright-cli")
            .unwrap();

        assert_eq!(browser.dependencies, vec!["shell", "node"]);
        assert_eq!(browser.capabilities, vec!["browser"]);
        assert!(browser.help_prompt.as_ref().unwrap().contains("browser automation"));
    }
}
```

- [ ] **Step 2: Run host resource tests to verify they fail**

Run: `cargo test -p ahandd plugin_runtime::host_resource`

Expected: compile failure because `get_host_resource` is not implemented.

- [ ] **Step 3: Implement host resource aggregation**

Add this implementation above the tests:

```rust
use std::collections::BTreeMap;

use super::builtin::builtin_registry;
use super::resource::{
    HostResourceSnapshot, HostResourceValue, InstalledPluginResource, PluginStatus,
};
use super::runtime_dir::RuntimeDirs;
use crate::browser_setup::{self, CheckStatus};

pub async fn get_host_resource() -> anyhow::Result<HostResourceSnapshot> {
    let registry = builtin_registry()?;
    let runtime = RuntimeDirs::new()?;
    let mut plugins = Vec::new();

    for manifest in registry.manifests() {
        let (status, resources) = inspect_plugin_resources(&manifest.id, &runtime).await;
        plugins.push(InstalledPluginResource {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            status,
            dependencies: manifest.dependencies.clone(),
            capabilities: manifest.capabilities.clone(),
            resources,
            help_prompt: manifest.help.as_ref().map(|help| help.prompt.clone()),
        });
    }

    Ok(HostResourceSnapshot {
        runtime_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        plugins,
    })
}

async fn inspect_plugin_resources(
    plugin_id: &str,
    runtime: &RuntimeDirs,
) -> (PluginStatus, BTreeMap<String, HostResourceValue>) {
    match plugin_id {
        "shell" => inspect_shell(),
        "file" => (PluginStatus::Installed, BTreeMap::new()),
        "node" => inspect_node().await,
        "python" => inspect_python(runtime).await,
        "browser-playwright-cli" => inspect_browser_playwright().await,
        _ => (PluginStatus::Missing, BTreeMap::new()),
    }
}

fn inspect_shell() -> (PluginStatus, BTreeMap<String, HostResourceValue>) {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let mut resources = BTreeMap::new();
    resources.insert(
        "shell".to_string(),
        HostResourceValue::Executable {
            name: "shell".to_string(),
            path: shell,
            version: None,
        },
    );
    (PluginStatus::Installed, resources)
}

async fn inspect_node() -> (PluginStatus, BTreeMap<String, HostResourceValue>) {
    let report = browser_setup::node::inspect().await;
    match report.status {
        CheckStatus::Ok { version, path, .. } => {
            let dirs = match RuntimeDirs::new() {
                Ok(dirs) => dirs,
                Err(_) => return (PluginStatus::Failed, BTreeMap::new()),
            };
            let mut resources = BTreeMap::new();
            resources.insert(
                "node".to_string(),
                HostResourceValue::Executable {
                    name: "node".to_string(),
                    path: path.to_string_lossy().into_owned(),
                    version: Some(version),
                },
            );
            resources.insert(
                "npm".to_string(),
                HostResourceValue::Executable {
                    name: "npm".to_string(),
                    path: dirs.npm_bin().to_string_lossy().into_owned(),
                    version: None,
                },
            );
            (PluginStatus::Installed, resources)
        }
        CheckStatus::Outdated { .. } => (PluginStatus::Outdated, BTreeMap::new()),
        CheckStatus::Failed { .. } => (PluginStatus::Failed, BTreeMap::new()),
        _ => (PluginStatus::Missing, BTreeMap::new()),
    }
}

async fn inspect_python(runtime: &RuntimeDirs) -> (PluginStatus, BTreeMap<String, HostResourceValue>) {
    let python = runtime.python_bin();
    if !python.exists() {
        return (PluginStatus::Missing, BTreeMap::new());
    }

    let version = tokio::process::Command::new(&python)
        .arg("--version")
        .output()
        .await
        .ok()
        .map(|out| {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let raw = if stdout.trim().is_empty() { stderr } else { stdout };
            raw.trim().to_string()
        });

    let mut resources = BTreeMap::new();
    resources.insert(
        "python".to_string(),
        HostResourceValue::Executable {
            name: "python".to_string(),
            path: python.to_string_lossy().into_owned(),
            version,
        },
    );
    (PluginStatus::Installed, resources)
}

async fn inspect_browser_playwright() -> (PluginStatus, BTreeMap<String, HostResourceValue>) {
    let node_report = browser_setup::node::inspect().await;
    if !matches!(node_report.status, CheckStatus::Ok { .. }) {
        return (PluginStatus::Blocked, BTreeMap::new());
    }

    let report = browser_setup::playwright::inspect().await;
    match report.status {
        CheckStatus::Ok { version, path, .. } => {
            let mut resources = BTreeMap::new();
            resources.insert(
                "playwrightCli".to_string(),
                HostResourceValue::Executable {
                    name: "playwright-cli".to_string(),
                    path: path.to_string_lossy().into_owned(),
                    version: Some(version),
                },
            );
            (PluginStatus::Installed, resources)
        }
        CheckStatus::Failed { .. } => (PluginStatus::Failed, BTreeMap::new()),
        CheckStatus::Outdated { .. } => (PluginStatus::Outdated, BTreeMap::new()),
        _ => (PluginStatus::Missing, BTreeMap::new()),
    }
}
```

- [ ] **Step 4: Verify Task 5**

Run: `cargo test -p ahandd plugin_runtime::host_resource`

Expected: host resource tests pass. On machines without managed Node, `node` may be `missing` and `browser-playwright-cli` may be `blocked`; the tests do not assert local install state.

- [ ] **Step 5: Commit Task 5**

```bash
git add crates/ahandd/src/plugin_runtime/host_resource.rs crates/ahandd/src/plugin_runtime/mod.rs
git commit -m "feat(ahandd): expose host resource snapshot"
```

---

### Task 6: Add Plugin CLI And Browser Setup Compatibility Aliases

**Files:**
- Modify: `crates/ahandd/src/main.rs`
- Modify: `crates/ahandd/src/browser_setup/mod.rs`
- Modify: `crates/ahandd/src/cli/browser_init.rs`
- Test: existing browser doctor integration test and the command smoke tests listed in Task 8

- [ ] **Step 1: Write alias tests for browser setup**

Add these tests to the existing test module in `crates/ahandd/src/browser_setup/mod.rs`:

```rust
#[tokio::test]
async fn inspect_accepts_plugin_id_aliases() {
    assert!(inspect("node").await.is_some());
    assert!(inspect("playwright").await.is_some());
    assert!(inspect("browser-playwright-cli").await.is_some());
}

#[tokio::test]
async fn run_step_rejects_file_plugin_because_it_has_no_installer() {
    let progress = |_: ProgressEvent| {};
    let err = run_step("file", false, progress).await.unwrap_err().to_string();
    assert!(err.contains("does not have an install step"));
}
```

- [ ] **Step 2: Run alias tests to verify they fail**

Run: `cargo test -p ahandd browser_setup::tests::inspect_accepts_plugin_id_aliases browser_setup::tests::run_step_rejects_file_plugin_because_it_has_no_installer`

Expected: `browser-playwright-cli` inspect returns `None` or `file` reports the old unknown-step message.

- [ ] **Step 3: Add compatibility aliases in `browser_setup::inspect`**

In `browser_setup/mod.rs`, update `inspect()`:

```rust
pub async fn inspect(name: &str) -> Option<CheckReport> {
    match name {
        "node" => Some(node::inspect().await),
        "playwright" | "browser-playwright-cli" => Some(playwright::inspect().await),
        "browser" => Some(inspect_browser()),
        _ => None,
    }
}
```

Update `run_step()` match arms:

```rust
        "playwright" | "browser-playwright-cli" => {
            let node_status = node::inspect().await;
            if !matches!(node_status.status, CheckStatus::Ok { .. }) {
                bail!(
                    "browser-playwright-cli requires node to be installed first. \
                     Run `ahandd browser-init --step node` first, or \
                     `ahandd plugin install browser-playwright-cli`."
                );
            }
            match playwright::ensure(force, progress_ref).await {
                Ok(r) => Ok(r),
                Err(e) => Err(wrap_failure(
                    e,
                    "browser-playwright-cli",
                    "playwright-cli",
                    progress_ref,
                )),
            }
        }
        "shell" | "file" | "python" => {
            bail!("plugin `{name}` does not have an install step in this release")
        }
```

Keep the existing `"node"` branch unchanged.

- [ ] **Step 4: Add `plugin` subcommands to `main.rs`**

Modify the `Cmd` enum in `crates/ahandd/src/main.rs`:

```rust
    /// Manage first-party aHand plugins
    Plugin {
        #[command(subcommand)]
        command: PluginCmd,
    },
```

Add a new enum near `Cmd`:

```rust
#[derive(Subcommand)]
enum PluginCmd {
    /// Inspect all plugins or a single plugin
    Doctor {
        plugin: Option<String>,
    },
    /// Install a plugin and required dependencies
    Install {
        plugin: String,
        #[arg(long)]
        force: bool,
    },
    /// Reinstall or repair a plugin
    Repair {
        plugin: String,
    },
    /// Print getHostResource JSON
    HostResource {
        #[arg(long)]
        json: bool,
    },
}
```

In the early subcommand match, add:

```rust
            Cmd::Plugin { command } => {
                return run_plugin_command(command).await;
            }
```

Add this helper below `main()`:

```rust
async fn run_plugin_command(command: &PluginCmd) -> anyhow::Result<()> {
    match command {
        PluginCmd::Doctor { plugin } => {
            let snapshot = plugin_runtime::get_host_resource().await?;
            for item in snapshot.plugins {
                if plugin.as_deref().is_some_and(|id| id != item.id) {
                    continue;
                }
                println!("{}: {:?}", item.id, item.status);
            }
            Ok(())
        }
        PluginCmd::Install { plugin, force } => {
            cli::browser_init::run(*force, Some(plugin.clone())).await
        }
        PluginCmd::Repair { plugin } => {
            cli::browser_init::run(true, Some(plugin.clone())).await
        }
        PluginCmd::HostResource { json } => {
            let snapshot = plugin_runtime::get_host_resource().await?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&snapshot)?);
            } else {
                for plugin in snapshot.plugins {
                    println!("{}: {:?}", plugin.id, plugin.status);
                }
            }
            Ok(())
        }
    }
}
```

- [ ] **Step 5: Keep browser-init readable for plugin ids**

In `crates/ahandd/src/cli/browser_init.rs`, replace the single-step success line:

```rust
println!("Step `{name}` complete.");
```

with:

```rust
println!("Plugin step `{name}` complete.");
```

No behavior changes are needed because `browser_setup::run_step()` now accepts both `playwright` and `browser-playwright-cli`.

- [ ] **Step 6: Verify Task 6**

Run:

```bash
cargo test -p ahandd browser_setup::tests
cargo run -p ahandd -- plugin host-resource --json
cargo run -p ahandd -- plugin doctor browser-playwright-cli
cargo run -p ahandd -- browser-doctor
```

Expected:

- tests pass.
- `plugin host-resource --json` prints JSON with `runtimeVersion` and `plugins`.
- `plugin doctor browser-playwright-cli` exits 0 and prints one plugin status.
- `browser-doctor` exits 0 or 1 depending on local setup, with no panic.

- [ ] **Step 7: Commit Task 6**

```bash
git add crates/ahandd/src/main.rs crates/ahandd/src/browser_setup/mod.rs crates/ahandd/src/cli/browser_init.rs
git commit -m "feat(ahandd): add plugin runtime cli"
```

---

### Task 7: Expose Host Resource Through Local Admin API

**Files:**
- Modify: `crates/ahandctl/Cargo.toml`
- Modify: `crates/ahandctl/src/admin.rs`
- Modify: `apps/admin/src/lib/api.ts`

- [ ] **Step 1: Add the ahandd library dependency**

Modify `crates/ahandctl/Cargo.toml`:

```toml
[dependencies]
ahand-protocol = { path = "../ahand-protocol" }
ahandd = { path = "../ahandd" }
tokio.workspace = true
```

Keep all other dependencies unchanged.

- [ ] **Step 2: Add admin route wiring**

In `crates/ahandctl/src/admin.rs`, add `.or(host_resource_route(token_arc.clone()))` to the API route chain after `status_route(...)`:

```rust
    let api = warp::path("api").and(
        status_route(token_arc.clone(), config_arc.clone())
            .or(host_resource_route(token_arc.clone()))
            .or(config_get_route(token_arc.clone(), config_arc.clone()))
            .or(config_put_route(token_arc.clone(), config_arc.clone()))
            .or(logs_route(token_arc.clone()))
            .or(runs_list_route(token_arc.clone()))
            .or(runs_get_route(token_arc.clone()))
            .or(runs_file_route(token_arc.clone()))
            .or(browser_init_route(token_arc.clone())),
    );
```

Add this route near `status_route`:

```rust
fn host_resource_route(
    token: Arc<String>,
) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
    warp::path!("host-resource")
        .and(warp::get())
        .and(with_auth(token))
        .and_then(|| async move {
            match ahandd::plugin_runtime::get_host_resource().await {
                Ok(snapshot) => Ok::<_, Rejection>(warp::reply::json(&snapshot)),
                Err(e) => {
                    eprintln!("Host resource error: {}", e);
                    Err(reject::reject())
                }
            }
        })
}
```

- [ ] **Step 3: Add TypeScript API types**

In `apps/admin/src/lib/api.ts`, add these interfaces below `StatusResponse`:

```ts
export type PluginStatus =
  | "installed"
  | "missing"
  | "outdated"
  | "failed"
  | "blocked";

export type HostResourceValue =
  | { kind: "executable"; name: string; path: string; version?: string }
  | { kind: "directory"; name: string; path: string }
  | { kind: "env"; name: string; value: string }
  | { kind: "config"; name: string; value: unknown };

export interface InstalledPluginResource {
  id: string;
  version: string;
  status: PluginStatus;
  dependencies: string[];
  capabilities: string[];
  resources: Record<string, HostResourceValue>;
  helpPrompt?: string;
}

export interface HostResourceSnapshot {
  runtimeVersion: string;
  platform: string;
  arch: string;
  plugins: InstalledPluginResource[];
}
```

Add this method to `api`:

```ts
  async getHostResource(): Promise<HostResourceSnapshot> {
    return fetchAPI("/host-resource");
  },
```

- [ ] **Step 4: Verify Task 7**

Run:

```bash
cargo check -p ahandctl
cargo check -p ahandd
pnpm --filter @ahand/admin build
```

Expected:

- both Rust packages compile.
- admin build completes without TypeScript errors.

- [ ] **Step 5: Commit Task 7**

```bash
git add crates/ahandctl/Cargo.toml crates/ahandctl/src/admin.rs apps/admin/src/lib/api.ts
git commit -m "feat(admin): expose host resource api"
```

---

### Task 8: Final Verification And Compatibility Pass

**Files:**
- Modify only files required by verification failures found in this task.

- [ ] **Step 1: Run focused Rust tests**

Run:

```bash
cargo test -p ahandd plugin_runtime
cargo test -p ahandd browser_setup
cargo test -p ahandd --test browser_doctor
```

Expected: all tests pass.

- [ ] **Step 2: Run package checks**

Run:

```bash
cargo check -p ahandd -p ahandctl
pnpm --filter @ahand/admin build
```

Expected: all checks pass.

- [ ] **Step 3: Run command smoke tests**

Run:

```bash
cargo run -p ahandd -- plugin host-resource --json
cargo run -p ahandd -- plugin doctor
cargo run -p ahandd -- browser-doctor
```

Expected:

- `plugin host-resource --json` prints valid JSON.
- `plugin doctor` prints statuses for `browser-playwright-cli`, `file`, `node`, `python`, and `shell`.
- `browser-doctor` exits 0 or 1 and does not panic.

- [ ] **Step 4: Inspect working tree**

Run:

```bash
git status --short
git diff --stat
```

Expected: only intentional files from this plan are modified.

- [ ] **Step 5: Commit final fixes if verification required changes**

If Step 1, Step 2, or Step 3 required fixes, commit those fixes:

```bash
git add crates/ahandd crates/ahandctl apps/admin
git commit -m "fix: stabilize plugin runtime stage one"
```

If no fixes were needed, do not create an empty commit.
