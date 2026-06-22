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

// NOTE: The static `npm` path below (`dependencies/node/bin/npm`) is the Unix
// layout.  On Windows there is no `npm` executable; `host_resource.rs` uses
// `RuntimeDirs::npm_invocation()` at runtime, which overrides this entry with
// the platform-correct invocation (`node.exe npm-cli.js`).
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
