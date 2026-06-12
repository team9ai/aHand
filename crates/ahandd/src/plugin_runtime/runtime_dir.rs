use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDirs {
    pub root: PathBuf,
}

impl RuntimeDirs {
    pub fn new() -> anyhow::Result<Self> {
        let home =
            dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
        Ok(Self::from_root(
            home.join(".cache")
                .join("ahand-runtimes")
                .join("ahand-primary-runtime"),
        ))
    }

    pub fn from_root(root: PathBuf) -> Self {
        Self { root }
    }

    // Plugin-runtime API surface; consumed by later plugin stages.
    #[allow(dead_code)]
    pub fn runtime_json(&self) -> PathBuf {
        self.root.join("runtime.json")
    }

    #[allow(dead_code)]
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
        let cli_bin = if cfg!(windows) {
            "playwright-cli.exe"
        } else {
            "playwright-cli"
        };
        assert_eq!(
            dirs.playwright_cli_bin(),
            std::path::PathBuf::from("/tmp/cache/ahand-primary-runtime/dependencies/node/bin")
                .join(cli_bin)
        );
    }

    #[test]
    fn runtime_dirs_new_uses_dot_cache_runtime_root() {
        let home = dirs::home_dir().unwrap();
        let dirs = RuntimeDirs::new().unwrap();

        assert_eq!(
            dirs.root,
            home.join(".cache")
                .join("ahand-runtimes")
                .join("ahand-primary-runtime")
        );
    }
}
