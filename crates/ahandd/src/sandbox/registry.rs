use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;

use super::types::{
    FileVersion, HostFileRef, NetworkPolicy, PermissionSnapshot, RegisteredExecEnvironment,
    RuntimeProviderConfig, SandboxError, SandboxFile, SandboxPermissionMode, SandboxResult,
    SandboxSessionConfig,
};

#[derive(Debug, Clone)]
pub struct SandboxSessionState {
    pub session_id: String,
    pub workspace_root: PathBuf,
    pub network: NetworkPolicy,
    pub runtimes: BTreeMap<String, RuntimeProviderConfig>,
    pub host_file_refs: BTreeMap<String, HostFileRef>,
    pub imported_files: BTreeMap<String, SandboxFile>,
    pub file_versions: BTreeMap<String, FileVersion>,
    permission: PermissionSnapshot,
}

impl SandboxSessionState {
    pub fn from_config(config: SandboxSessionConfig) -> Self {
        Self {
            session_id: config.session_id,
            workspace_root: config.workspace_root,
            network: config.network,
            runtimes: BTreeMap::new(),
            host_file_refs: BTreeMap::new(),
            imported_files: BTreeMap::new(),
            file_versions: BTreeMap::new(),
            permission: PermissionSnapshot {
                mode: config.permission_mode,
                version: 1,
            },
        }
    }

    pub fn permission_snapshot(&self) -> PermissionSnapshot {
        self.permission.clone()
    }

    pub fn update_permission(&mut self, mode: SandboxPermissionMode) -> PermissionSnapshot {
        if self.permission.mode != mode {
            self.permission = PermissionSnapshot {
                mode,
                version: self.permission.version + 1,
            };
        }
        self.permission.clone()
    }

    pub fn exec_environment(&self) -> RegisteredExecEnvironment {
        let mut path_entries = Vec::new();
        let mut readonly_roots = Vec::new();
        let mut env = HashMap::new();

        for provider in self.runtimes.values() {
            if let Some(parent) = provider.executable.parent() {
                push_unique_path(&mut path_entries, parent.to_path_buf());
            }
            for root in &provider.readonly_roots {
                push_unique_path(&mut readonly_roots, root.clone());
            }
            for (key, value) in &provider.env {
                env.insert(key.clone(), value.clone());
            }
        }

        path_entries.sort();
        readonly_roots.sort();

        RegisteredExecEnvironment {
            path_entries,
            readonly_roots,
            env,
            default_timeout: Duration::from_secs(30),
        }
    }
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

#[derive(Debug, Default)]
pub struct SandboxRegistry {
    sessions: BTreeMap<String, SandboxSessionState>,
}

impl SandboxRegistry {
    pub fn create_session(&mut self, config: SandboxSessionConfig) -> SandboxResult<()> {
        let workspace_root = config.workspace_root.canonicalize().map_err(|e| {
            SandboxError::unavailable(format!("failed to resolve sandbox workspace root: {e}"))
        })?;
        let session_id = config.session_id.clone();
        let config = SandboxSessionConfig {
            workspace_root,
            ..config
        };
        self.sessions
            .insert(session_id, SandboxSessionState::from_config(config));
        Ok(())
    }

    pub fn session(&self, session_id: &str) -> SandboxResult<&SandboxSessionState> {
        self.sessions.get(session_id).ok_or_else(|| {
            SandboxError::unavailable(format!("sandbox session '{session_id}' does not exist"))
        })
    }

    pub fn session_mut(&mut self, session_id: &str) -> SandboxResult<&mut SandboxSessionState> {
        self.sessions.get_mut(session_id).ok_or_else(|| {
            SandboxError::unavailable(format!("sandbox session '{session_id}' does not exist"))
        })
    }

    pub fn permission_snapshot(&self, session_id: &str) -> SandboxResult<PermissionSnapshot> {
        Ok(self.session(session_id)?.permission_snapshot())
    }

    pub fn update_permission(
        &mut self,
        session_id: &str,
        mode: SandboxPermissionMode,
    ) -> SandboxResult<PermissionSnapshot> {
        Ok(self.session_mut(session_id)?.update_permission(mode))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::types::{NetworkPolicy, SandboxPermissionMode, SandboxSessionConfig};
    use std::fs;
    use std::path::PathBuf;

    fn config(workspace_root: PathBuf) -> SandboxSessionConfig {
        SandboxSessionConfig {
            session_id: "session-1".to_string(),
            permission_mode: SandboxPermissionMode::Readonly,
            workspace_root,
            network: NetworkPolicy::Enabled,
        }
    }

    #[test]
    fn create_session_initializes_permission_version() {
        let temp = tempfile::tempdir().unwrap();
        let mut registry = SandboxRegistry::default();

        registry.create_session(config(temp.path().into())).unwrap();
        let snapshot = registry.permission_snapshot("session-1").unwrap();

        assert_eq!(snapshot.mode, SandboxPermissionMode::Readonly);
        assert_eq!(snapshot.version, 1);
    }

    #[test]
    fn create_session_canonicalizes_workspace_root() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("workspace");
        fs::create_dir_all(&workspace_root).unwrap();
        let root_with_parent_component = workspace_root.join("..").join("workspace");
        let mut registry = SandboxRegistry::default();

        registry
            .create_session(config(root_with_parent_component))
            .unwrap();

        assert_eq!(
            registry.session("session-1").unwrap().workspace_root,
            workspace_root.canonicalize().unwrap()
        );
    }

    #[test]
    fn permission_update_increments_only_on_change() {
        let temp = tempfile::tempdir().unwrap();
        let mut registry = SandboxRegistry::default();
        registry.create_session(config(temp.path().into())).unwrap();

        let same = registry
            .update_permission("session-1", SandboxPermissionMode::Readonly)
            .unwrap();
        let changed = registry
            .update_permission("session-1", SandboxPermissionMode::Full)
            .unwrap();

        assert_eq!(same.version, 1);
        assert_eq!(changed.version, 2);
        assert_eq!(changed.mode, SandboxPermissionMode::Full);
    }

    #[test]
    fn exec_environment_aggregates_provider_paths_roots_and_env() {
        let temp = tempfile::tempdir().unwrap();
        let python_root = temp.path().join("python");
        let node_root = temp.path().join("node");
        let python_bin = python_root.join("bin");
        let node_bin = node_root.join("bin");
        fs::create_dir_all(&python_bin).unwrap();
        fs::create_dir_all(&node_bin).unwrap();
        fs::write(python_bin.join("python"), "").unwrap();
        fs::write(node_bin.join("node"), "").unwrap();

        let mut session = SandboxSessionState::from_config(config(temp.path().into()));
        session.runtimes.insert(
            "python".to_string(),
            RuntimeProviderConfig {
                name: "python".to_string(),
                executable: python_bin.join("python").canonicalize().unwrap(),
                readonly_roots: vec![python_root.canonicalize().unwrap()],
                env: std::collections::HashMap::from([(
                    "PYTHONNOUSERSITE".to_string(),
                    "1".to_string(),
                )]),
                default_timeout: std::time::Duration::from_secs(11),
            },
        );
        session.runtimes.insert(
            "node".to_string(),
            RuntimeProviderConfig {
                name: "node".to_string(),
                executable: node_bin.join("node").canonicalize().unwrap(),
                readonly_roots: vec![node_root.canonicalize().unwrap()],
                env: std::collections::HashMap::from([(
                    "NODE_PATH".to_string(),
                    node_root.join("node_modules").to_string_lossy().to_string(),
                )]),
                default_timeout: std::time::Duration::from_secs(17),
            },
        );

        let env = session.exec_environment();

        assert_eq!(
            env.path_entries,
            vec![
                node_bin.canonicalize().unwrap(),
                python_bin.canonicalize().unwrap()
            ]
        );
        assert_eq!(
            env.readonly_roots,
            vec![
                node_root.canonicalize().unwrap(),
                python_root.canonicalize().unwrap()
            ]
        );
        assert_eq!(env.env["PYTHONNOUSERSITE"], "1");
        assert!(env.env["NODE_PATH"].contains("node_modules"));
        assert_eq!(env.default_timeout, std::time::Duration::from_secs(30));
    }

    #[test]
    fn exec_environment_deduplicates_provider_paths_and_roots() {
        let temp = tempfile::tempdir().unwrap();
        let runtime_root = temp.path().join("runtime");
        let runtime_bin = runtime_root.join("bin");
        fs::create_dir_all(&runtime_bin).unwrap();
        fs::write(runtime_bin.join("python"), "").unwrap();
        fs::write(runtime_bin.join("node"), "").unwrap();

        let mut session = SandboxSessionState::from_config(config(temp.path().into()));
        for name in ["python", "node"] {
            session.runtimes.insert(
                name.to_string(),
                RuntimeProviderConfig {
                    name: name.to_string(),
                    executable: runtime_bin.join(name).canonicalize().unwrap(),
                    readonly_roots: vec![runtime_root.canonicalize().unwrap()],
                    env: std::collections::HashMap::new(),
                    default_timeout: std::time::Duration::from_secs(30),
                },
            );
        }

        let env = session.exec_environment();

        assert_eq!(env.path_entries, vec![runtime_bin.canonicalize().unwrap()]);
        assert_eq!(
            env.readonly_roots,
            vec![runtime_root.canonicalize().unwrap()]
        );
    }
}
