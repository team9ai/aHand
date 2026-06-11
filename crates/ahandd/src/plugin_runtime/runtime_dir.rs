use std::ffi::OsString;
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
        self.node_dir()
            .join("bin")
            .join(ahand_platform::paths::exe_name("node"))
    }

    /// Return the program + leading args needed to invoke npm on the current
    /// platform:
    ///
    /// - **Unix**: `(node/bin/npm, [])`
    /// - **Windows**: `(node/bin/node.exe, [node_dir/node_modules/npm/bin/npm-cli.js])`
    ///
    /// Pass the returned leading args *before* the actual npm args when
    /// constructing a `Command`.
    pub fn npm_invocation(&self) -> (PathBuf, Vec<OsString>) {
        let node_dir = self.node_dir();
        if cfg!(windows) {
            let program = node_dir
                .join("bin")
                .join(ahand_platform::paths::exe_name("node"));
            let npm_cli = node_dir
                .join("node_modules")
                .join("npm")
                .join("bin")
                .join("npm-cli.js");
            (program, vec![npm_cli.into()])
        } else {
            let program = node_dir.join("bin").join("npm");
            (program, vec![])
        }
    }

    /// Return the playwright-cli binary path (unix: `node/bin/playwright-cli`;
    /// windows: `node/bin/playwright-cli.exe`).
    ///
    /// On Windows, use `playwright_cli_invocation()` to spawn the CLI because
    /// the installed CLI is a JS file, not a native executable.
    pub fn playwright_cli_bin(&self) -> PathBuf {
        self.node_dir()
            .join("bin")
            .join(ahand_platform::paths::exe_name("playwright-cli"))
    }

    /// Return the program + leading args needed to invoke playwright-cli.
    ///
    /// On Unix the CLI is a native wrapper at `node/bin/playwright-cli`, so the
    /// invocation is just `(node/bin/playwright-cli, [])`.
    ///
    /// On Windows `npm install -g --prefix <node_dir>` creates shims at
    /// `<node_dir>\playwright-cli.cmd` but no native exe. The actual JS entry
    /// lives at `<node_dir>\node_modules\@playwright\cli\<bin>`. We resolve it
    /// by reading the package.json `"bin"` map at invocation time; if that fails
    /// we fall back to the conventional path
    /// `node_modules/@playwright/cli/cli.js`. If neither exists we return an
    /// error so callers can surface `CheckStatus::Missing`.
    pub fn playwright_cli_invocation(&self) -> anyhow::Result<(PathBuf, Vec<OsString>)> {
        let node_dir = self.node_dir();
        if cfg!(windows) {
            let program = node_dir
                .join("bin")
                .join(ahand_platform::paths::exe_name("node"));
            let entry = resolve_playwright_cli_entry(&node_dir)?;
            Ok((program, vec![entry.into()]))
        } else {
            Ok((node_dir.join("bin").join("playwright-cli"), vec![]))
        }
    }

    pub fn python_dir(&self) -> PathBuf {
        self.dependencies_dir().join("python")
    }

    pub fn python_bin(&self) -> PathBuf {
        self.python_dir()
            .join("bin")
            .join(ahand_platform::paths::exe_name("python3"))
    }
}

/// Resolve the playwright-cli JS entry script under the npm global prefix.
///
/// Strategy:
/// 1. Read `node_modules/@playwright/cli/package.json` and parse the `"bin"`
///    mapping to find the CLI entry script.
/// 2. Fall back to the conventional path `node_modules/@playwright/cli/cli.js`.
/// 3. If neither exists, return an error (caller should surface Missing).
fn resolve_playwright_cli_entry(node_dir: &std::path::Path) -> anyhow::Result<PathBuf> {
    let pkg_dir = node_dir
        .join("node_modules")
        .join("@playwright")
        .join("cli");

    // Attempt 1: read package.json "bin" mapping.
    let pkg_json = pkg_dir.join("package.json");
    if let Ok(contents) = std::fs::read_to_string(&pkg_json)
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(&contents)
    {
        // "bin" can be a string (single entry) or an object (map).
        let entry_rel = val.get("bin").and_then(|b| {
            if let Some(s) = b.as_str() {
                Some(s.to_owned())
            } else if let Some(obj) = b.as_object() {
                // Pick the first value (typically "playwright-cli" key).
                obj.values()
                    .next()
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            } else {
                None
            }
        });

        if let Some(rel) = entry_rel {
            // Normalise: strip leading "./" if present.
            let rel = rel.trim_start_matches("./");
            let candidate = pkg_dir.join(rel);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    // Attempt 2: conventional path.
    let conventional = pkg_dir.join("cli.js");
    if conventional.exists() {
        return Ok(conventional);
    }

    anyhow::bail!(
        "playwright-cli entry script not found under {}; \
         run `ahandd browser-init --step playwright` to install it",
        node_dir.display()
    )
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

    // -------------------------------------------------------------------------
    // npm_invocation shape tests
    // -------------------------------------------------------------------------

    #[test]
    fn npm_invocation_unix_shape() {
        #[cfg(not(windows))]
        {
            let root = std::path::PathBuf::from("/tmp/rt");
            let dirs = RuntimeDirs::from_root(root);
            let (prog, leading) = dirs.npm_invocation();
            // Unix: program is node/bin/npm, no leading args.
            assert!(
                prog.ends_with("bin/npm"),
                "unix npm program should be bin/npm, got: {}",
                prog.display()
            );
            assert!(
                leading.is_empty(),
                "unix npm invocation has no leading args"
            );
        }
    }

    #[test]
    fn npm_invocation_windows_shape() {
        #[cfg(windows)]
        {
            let root = std::path::PathBuf::from(r"C:\rt");
            let dirs = RuntimeDirs::from_root(root);
            let (prog, leading) = dirs.npm_invocation();
            // Windows: program is node.exe, leading arg is npm-cli.js path.
            assert!(
                prog.file_name().unwrap() == "node.exe",
                "windows npm program should be node.exe, got: {}",
                prog.display()
            );
            assert_eq!(leading.len(), 1, "windows npm invocation has 1 leading arg");
            let cli_arg = std::path::PathBuf::from(&leading[0]);
            assert!(
                cli_arg.ends_with("npm/bin/npm-cli.js"),
                "leading arg should point to npm-cli.js, got: {}",
                cli_arg.display()
            );
        }
    }

    // -------------------------------------------------------------------------
    // playwright_cli_invocation shape tests
    // -------------------------------------------------------------------------

    #[test]
    fn playwright_cli_invocation_unix_shape() {
        #[cfg(not(windows))]
        {
            let dir = tempfile::tempdir().unwrap();
            let node_dir = dir.path().join("node");
            let bin_dir = node_dir.join("bin");
            std::fs::create_dir_all(&bin_dir).unwrap();
            // Create the unix CLI binary so the path exists.
            std::fs::write(bin_dir.join("playwright-cli"), b"#!/bin/sh\n").unwrap();

            // Build a RuntimeDirs whose node_dir is dir/dependencies/node.
            // We need the full root structure.
            let root = dir.path().to_path_buf();
            // Manually mirror RuntimeDirs.node_dir() = root/dependencies/node
            let full_node_dir = root.join("dependencies").join("node");
            let full_bin_dir = full_node_dir.join("bin");
            std::fs::create_dir_all(&full_bin_dir).unwrap();
            std::fs::write(full_bin_dir.join("playwright-cli"), b"#!/bin/sh\n").unwrap();

            let rt = RuntimeDirs::from_root(root);
            let (prog, leading) = rt.playwright_cli_invocation().unwrap();
            assert!(
                prog.file_name().unwrap() == "playwright-cli",
                "unix invocation program should be playwright-cli, got: {}",
                prog.display()
            );
            assert!(leading.is_empty(), "unix invocation has no leading args");
        }
    }

    #[test]
    fn playwright_cli_invocation_windows_package_json_bin_string() {
        // Simulate a Windows-layout fixture: node_dir/node_modules/@playwright/cli/
        // with a package.json whose "bin" is a string pointing to cli.js.
        let dir = tempfile::tempdir().unwrap();
        let node_dir = dir.path().join("dependencies").join("node");
        let pkg_dir = node_dir
            .join("node_modules")
            .join("@playwright")
            .join("cli");
        std::fs::create_dir_all(&pkg_dir).unwrap();

        // Create the entry script.
        std::fs::write(pkg_dir.join("cli.js"), b"// cli entry").unwrap();

        // Write package.json with bin as a string.
        std::fs::write(
            pkg_dir.join("package.json"),
            r#"{"name":"@playwright/cli","bin":"./cli.js"}"#,
        )
        .unwrap();

        let entry = resolve_playwright_cli_entry(&node_dir).unwrap();
        assert!(
            entry.ends_with("cli.js"),
            "bin-string mapping should resolve to cli.js, got: {}",
            entry.display()
        );
    }

    #[test]
    fn playwright_cli_invocation_windows_package_json_bin_object() {
        let dir = tempfile::tempdir().unwrap();
        let node_dir = dir.path().join("dependencies").join("node");
        let pkg_dir = node_dir
            .join("node_modules")
            .join("@playwright")
            .join("cli");
        std::fs::create_dir_all(&pkg_dir).unwrap();

        std::fs::write(pkg_dir.join("index.js"), b"// cli entry").unwrap();

        // Write package.json with bin as an object.
        std::fs::write(
            pkg_dir.join("package.json"),
            r#"{"name":"@playwright/cli","bin":{"playwright-cli":"./index.js"}}"#,
        )
        .unwrap();

        let entry = resolve_playwright_cli_entry(&node_dir).unwrap();
        assert!(
            entry.ends_with("index.js"),
            "bin-object mapping should resolve to index.js, got: {}",
            entry.display()
        );
    }

    #[test]
    fn playwright_cli_invocation_windows_fallback_to_conventional() {
        // No package.json — should fall back to cli.js.
        let dir = tempfile::tempdir().unwrap();
        let node_dir = dir.path().join("dependencies").join("node");
        let pkg_dir = node_dir
            .join("node_modules")
            .join("@playwright")
            .join("cli");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(pkg_dir.join("cli.js"), b"// cli fallback").unwrap();

        let entry = resolve_playwright_cli_entry(&node_dir).unwrap();
        assert!(
            entry.ends_with("cli.js"),
            "fallback should resolve to cli.js, got: {}",
            entry.display()
        );
    }

    #[test]
    fn playwright_cli_invocation_windows_returns_error_when_missing() {
        // Neither package.json nor cli.js exist.
        let dir = tempfile::tempdir().unwrap();
        let node_dir = dir.path().join("dependencies").join("node");
        // Don't create any files under pkg_dir.

        let result = resolve_playwright_cli_entry(&node_dir);
        assert!(
            result.is_err(),
            "should return error when entry script is missing"
        );
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("not found") || msg.contains("browser-init"),
            "error message should guide the user: {msg}"
        );
    }
}
