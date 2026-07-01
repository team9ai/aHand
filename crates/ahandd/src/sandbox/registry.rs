use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::Duration;

use super::mounts;
use super::types::{
    FileVersion, HostFileRef, MountScope, NetworkPolicy, PermissionSnapshot,
    RegisteredExecEnvironment, RegisteredSandboxMount, RuntimeProviderConfig, SandboxError,
    SandboxFile, SandboxInvocationContext, SandboxMountSpec, SandboxPermissionMode, SandboxResult,
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
    pub mounts: BTreeMap<(String, MountScope), RegisteredSandboxMount>,
    permission: PermissionSnapshot,
}

impl SandboxSessionState {
    pub fn from_config(config: SandboxSessionConfig) -> Self {
        let SandboxSessionConfig {
            session_id,
            permission_mode,
            workspace_root,
            network,
            mounts: _,
        } = config;

        Self {
            session_id,
            workspace_root,
            network,
            runtimes: BTreeMap::new(),
            host_file_refs: BTreeMap::new(),
            imported_files: BTreeMap::new(),
            file_versions: BTreeMap::new(),
            mounts: BTreeMap::new(),
            permission: PermissionSnapshot {
                mode: permission_mode,
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
        self.exec_environment_for(None)
    }

    pub fn exec_environment_for(
        &self,
        context: Option<&SandboxInvocationContext>,
    ) -> RegisteredExecEnvironment {
        let mut path_entries = Vec::new();
        let mut readonly_roots = Vec::new();
        let mut env = HashMap::new();
        let mut active_mounts = Vec::new();

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

        for mount in self.mounts.values() {
            let active = mount_scope_active(&mount.scope, context);
            if active {
                if let Some(env_var) = &mount.env_var {
                    env.insert(env_var.clone(), mount.target.to_string_lossy().to_string());
                }
                active_mounts.push(mount.clone());
            }
        }

        path_entries.sort();
        readonly_roots.sort();

        RegisteredExecEnvironment {
            path_entries,
            readonly_roots,
            env,
            mounts: active_mounts,
            default_timeout: Duration::from_secs(30),
        }
    }

    pub fn registered_mount_env_vars(&self) -> BTreeSet<String> {
        self.mounts
            .values()
            .filter_map(|mount| mount.env_var.clone())
            .collect()
    }
}

fn mount_scope_active(scope: &MountScope, context: Option<&SandboxInvocationContext>) -> bool {
    match scope {
        MountScope::Session => true,
        MountScope::Run { run_id } => {
            context.and_then(|context| context.run_id.as_deref().or(context.scope_id.as_deref()))
                == Some(run_id.as_str())
        }
        MountScope::Invocation { invocation_id } => {
            context.and_then(|context| context.invocation_id.as_deref())
                == Some(invocation_id.as_str())
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
        let mut config = SandboxSessionConfig {
            workspace_root,
            ..config
        };
        let initial_mounts = std::mem::take(&mut config.mounts);
        let mut session = SandboxSessionState::from_config(config);
        for spec in initial_mounts {
            Self::register_mount_for_session(&mut session, spec)?;
        }
        self.sessions.insert(session_id, session);
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

    pub fn register_mount(
        &mut self,
        session_id: &str,
        spec: SandboxMountSpec,
    ) -> SandboxResult<RegisteredSandboxMount> {
        let session = self.session_mut(session_id)?;
        Self::register_mount_for_session(session, spec)
    }

    pub fn unregister_mount(
        &mut self,
        session_id: &str,
        mount_id: &str,
        scope: MountScope,
    ) -> SandboxResult<()> {
        let session = self.session_mut(session_id)?;
        let key = (mount_id.to_string(), scope);
        session.mounts.remove(&key).map(|_| ()).ok_or_else(|| {
            SandboxError::mount_not_registered(format!(
                "sandbox mount '{mount_id}' is not registered for the requested scope"
            ))
        })
    }

    pub fn list_mounts(&self, session_id: &str) -> SandboxResult<Vec<RegisteredSandboxMount>> {
        Ok(self.session(session_id)?.mounts.values().cloned().collect())
    }

    fn register_mount_for_session(
        session: &mut SandboxSessionState,
        spec: SandboxMountSpec,
    ) -> SandboxResult<RegisteredSandboxMount> {
        let key = (spec.mount_id.clone(), spec.scope.clone());
        if session.mounts.contains_key(&key) {
            return Err(SandboxError::mount_already_registered(format!(
                "sandbox mount '{}' is already registered for the requested scope",
                spec.mount_id
            )));
        }
        if let Some(env_var) = &spec.env_var {
            if session
                .mounts
                .values()
                .any(|mount| mount.env_var.as_deref() == Some(env_var.as_str()))
            {
                return Err(SandboxError::mount_env_conflict(format!(
                    "sandbox mount env var '{env_var}' is already registered"
                )));
            }
        }
        let existing_targets = session
            .mounts
            .values()
            .map(|mount| mount.target.as_path())
            .collect::<Vec<_>>();
        let registered =
            mounts::register_mount_with_existing_targets(session, spec, existing_targets)?;
        session.mounts.insert(key, registered.clone());
        Ok(registered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::types::{
        MountAccess, MountScope, MountSource, NetworkPolicy, SandboxInvocationContext,
        SandboxMountSpec, SandboxPermissionMode, SandboxSessionConfig,
    };
    use std::fs;
    use std::path::PathBuf;

    fn config(workspace_root: PathBuf) -> SandboxSessionConfig {
        SandboxSessionConfig {
            session_id: "session-1".to_string(),
            permission_mode: SandboxPermissionMode::Readonly,
            workspace_root,
            network: NetworkPolicy::Enabled,
            mounts: Vec::new(),
        }
    }

    fn readonly_mount(mount_id: &str, source: PathBuf, scope: MountScope) -> SandboxMountSpec {
        SandboxMountSpec {
            mount_id: mount_id.to_string(),
            source: MountSource::HostPath(source),
            access: MountAccess::ReadOnly,
            scope,
            target: None,
            env_var: None,
        }
    }

    fn readonly_mount_with_env(
        mount_id: &str,
        source: PathBuf,
        scope: MountScope,
        env_var: &str,
    ) -> SandboxMountSpec {
        SandboxMountSpec {
            env_var: Some(env_var.to_string()),
            ..readonly_mount(mount_id, source, scope)
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
    fn sandbox_registry_create_session_registers_initial_config_mounts() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        fs::create_dir_all(&workspace_root).unwrap();
        fs::create_dir_all(&source).unwrap();
        let mut config = config(workspace_root.clone());
        config.mounts = vec![readonly_mount(
            "selected-folder",
            source.clone(),
            MountScope::Session,
        )];
        let mut registry = SandboxRegistry::default();

        registry.create_session(config).unwrap();
        let mounts = registry.list_mounts("session-1").unwrap();

        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].mount_id, "selected-folder");
        assert_eq!(
            mounts[0].source,
            MountSource::HostPath(source.canonicalize().unwrap())
        );
        assert_eq!(
            mounts[0].target,
            workspace_root
                .canonicalize()
                .unwrap()
                .join("workspace/mounts/selected-folder")
        );
    }

    #[test]
    fn sandbox_registry_create_session_fails_when_initial_mount_invalid() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        fs::create_dir_all(&workspace_root).unwrap();
        let mut config = config(workspace_root);
        config.mounts = vec![readonly_mount(
            "selected-folder",
            temp.path().join("missing"),
            MountScope::Session,
        )];
        let mut registry = SandboxRegistry::default();

        let err = registry.create_session(config).unwrap_err();

        assert_eq!(err.code, "MOUNT_SOURCE_NOT_FOUND");
        assert!(registry.session("session-1").is_err());
    }

    #[test]
    fn sandbox_registry_register_list_unregister_mount_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        fs::create_dir_all(&workspace_root).unwrap();
        fs::create_dir_all(&source).unwrap();
        let mut registry = SandboxRegistry::default();
        registry.create_session(config(workspace_root)).unwrap();

        let registered = registry
            .register_mount(
                "session-1",
                readonly_mount(
                    "selected-folder",
                    source,
                    MountScope::Run {
                        run_id: "run-1".to_string(),
                    },
                ),
            )
            .unwrap();
        assert_eq!(registry.list_mounts("session-1").unwrap(), vec![registered]);

        registry
            .unregister_mount(
                "session-1",
                "selected-folder",
                MountScope::Run {
                    run_id: "run-1".to_string(),
                },
            )
            .unwrap();

        assert!(registry.list_mounts("session-1").unwrap().is_empty());
        let err = registry
            .unregister_mount("session-1", "selected-folder", MountScope::Session)
            .unwrap_err();
        assert_eq!(err.code, "MOUNT_NOT_REGISTERED");
    }

    #[test]
    fn sandbox_registry_duplicate_mount_id_same_scope_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        fs::create_dir_all(&workspace_root).unwrap();
        fs::create_dir_all(&source).unwrap();
        let mut registry = SandboxRegistry::default();
        registry.create_session(config(workspace_root)).unwrap();
        let scope = MountScope::Run {
            run_id: "run-1".to_string(),
        };
        registry
            .register_mount(
                "session-1",
                readonly_mount("selected-folder", source.clone(), scope.clone()),
            )
            .unwrap();

        let err = registry
            .register_mount(
                "session-1",
                readonly_mount("selected-folder", source, scope),
            )
            .unwrap_err();

        assert_eq!(err.code, "MOUNT_ALREADY_REGISTERED");
    }

    #[test]
    fn sandbox_registry_duplicate_mount_id_different_scope_is_allowed_with_auto_suffixes() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        fs::create_dir_all(&workspace_root).unwrap();
        fs::create_dir_all(&source).unwrap();
        let mut registry = SandboxRegistry::default();
        registry
            .create_session(config(workspace_root.clone()))
            .unwrap();

        let first = registry
            .register_mount(
                "session-1",
                readonly_mount(
                    "selected-folder",
                    source.clone(),
                    MountScope::Run {
                        run_id: "run-1".to_string(),
                    },
                ),
            )
            .unwrap();
        let second = registry
            .register_mount(
                "session-1",
                readonly_mount(
                    "selected-folder",
                    source,
                    MountScope::Run {
                        run_id: "run-2".to_string(),
                    },
                ),
            )
            .unwrap();

        let namespace = workspace_root
            .canonicalize()
            .unwrap()
            .join("workspace/mounts");
        assert_eq!(first.target, namespace.join("selected-folder"));
        assert_eq!(second.target, namespace.join("selected-folder-2"));
        assert_eq!(
            registry.list_mounts("session-1").unwrap(),
            vec![first, second]
        );
    }

    #[test]
    fn sandbox_registry_explicit_target_conflicts_with_registered_mount() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        fs::create_dir_all(&workspace_root).unwrap();
        fs::create_dir_all(&source).unwrap();
        let mut registry = SandboxRegistry::default();
        registry.create_session(config(workspace_root)).unwrap();
        registry
            .register_mount(
                "session-1",
                readonly_mount("selected-folder", source.clone(), MountScope::Session),
            )
            .unwrap();
        let mut spec = readonly_mount(
            "other-folder",
            source,
            MountScope::Run {
                run_id: "run-1".to_string(),
            },
        );
        spec.target = Some(PathBuf::from("workspace/mounts/selected-folder"));

        let err = registry.register_mount("session-1", spec).unwrap_err();

        assert_eq!(err.code, "MOUNT_TARGET_CONFLICT");
    }

    #[test]
    fn sandbox_registry_duplicate_mount_env_var_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        fs::create_dir_all(&workspace_root).unwrap();
        fs::create_dir_all(&source).unwrap();
        let mut registry = SandboxRegistry::default();
        registry.create_session(config(workspace_root)).unwrap();
        registry
            .register_mount(
                "session-1",
                readonly_mount_with_env(
                    "selected-folder",
                    source.clone(),
                    MountScope::Run {
                        run_id: "run-1".to_string(),
                    },
                    "COFFICE_SELECTED_FOLDER_DIR",
                ),
            )
            .unwrap();

        let err = registry
            .register_mount(
                "session-1",
                readonly_mount_with_env(
                    "selected-folder-2",
                    source,
                    MountScope::Run {
                        run_id: "run-2".to_string(),
                    },
                    "COFFICE_SELECTED_FOLDER_DIR",
                ),
            )
            .unwrap_err();

        assert_eq!(err.code, "MOUNT_ENV_CONFLICT");
    }

    #[test]
    fn sandbox_registry_create_session_rejects_initial_duplicate_mount_env_var() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        fs::create_dir_all(&workspace_root).unwrap();
        fs::create_dir_all(&source).unwrap();
        let mut config = config(workspace_root);
        config.mounts = vec![
            readonly_mount_with_env(
                "selected-folder",
                source.clone(),
                MountScope::Run {
                    run_id: "run-1".to_string(),
                },
                "COFFICE_SELECTED_FOLDER_DIR",
            ),
            readonly_mount_with_env(
                "selected-folder-2",
                source,
                MountScope::Run {
                    run_id: "run-2".to_string(),
                },
                "COFFICE_SELECTED_FOLDER_DIR",
            ),
        ];
        let mut registry = SandboxRegistry::default();

        let err = registry.create_session(config).unwrap_err();

        assert_eq!(err.code, "MOUNT_ENV_CONFLICT");
        assert!(registry.session("session-1").is_err());
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

    #[test]
    fn sandbox_exec_environment_session_mount_is_active_without_context() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        fs::create_dir_all(&workspace_root).unwrap();
        fs::create_dir_all(&source).unwrap();
        let mut registry = SandboxRegistry::default();
        registry
            .create_session(config(workspace_root.clone()))
            .unwrap();
        registry
            .register_mount(
                "session-1",
                readonly_mount_with_env(
                    "selected-folder",
                    source,
                    MountScope::Session,
                    "COFFICE_SELECTED_FOLDER_DIR",
                ),
            )
            .unwrap();

        let env = registry
            .session("session-1")
            .unwrap()
            .exec_environment_for(None);

        let target = workspace_root
            .canonicalize()
            .unwrap()
            .join("workspace/mounts/selected-folder");
        assert_eq!(env.mounts.len(), 1);
        assert_eq!(env.mounts[0].target, target);
        assert_eq!(
            env.env["COFFICE_SELECTED_FOLDER_DIR"],
            target.to_string_lossy()
        );
    }

    #[test]
    fn sandbox_exec_environment_run_mount_requires_matching_run_id() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        fs::create_dir_all(&workspace_root).unwrap();
        fs::create_dir_all(&source).unwrap();
        let mut registry = SandboxRegistry::default();
        registry
            .create_session(config(workspace_root.clone()))
            .unwrap();
        registry
            .register_mount(
                "session-1",
                readonly_mount_with_env(
                    "selected-folder",
                    source,
                    MountScope::Run {
                        run_id: "run-1".to_string(),
                    },
                    "COFFICE_SELECTED_FOLDER_DIR",
                ),
            )
            .unwrap();

        let matching = registry
            .session("session-1")
            .unwrap()
            .exec_environment_for(Some(&SandboxInvocationContext {
                session_id: "session-1".to_string(),
                run_id: Some("run-1".to_string()),
                scope_id: None,
                invocation_id: None,
            }));
        let mismatched = registry
            .session("session-1")
            .unwrap()
            .exec_environment_for(Some(&SandboxInvocationContext {
                session_id: "session-1".to_string(),
                run_id: Some("run-2".to_string()),
                scope_id: None,
                invocation_id: None,
            }));
        let no_context = registry
            .session("session-1")
            .unwrap()
            .exec_environment_for(None);

        let target = workspace_root
            .canonicalize()
            .unwrap()
            .join("workspace/mounts/selected-folder");
        assert_eq!(
            matching.env["COFFICE_SELECTED_FOLDER_DIR"],
            target.to_string_lossy()
        );
        assert_eq!(matching.mounts.len(), 1);
        assert!(mismatched.mounts.is_empty());
        assert!(!mismatched.env.contains_key("COFFICE_SELECTED_FOLDER_DIR"));
        assert!(no_context.mounts.is_empty());
        assert!(!no_context.env.contains_key("COFFICE_SELECTED_FOLDER_DIR"));
    }

    #[test]
    fn sandbox_exec_environment_invocation_mount_requires_matching_invocation_id() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        fs::create_dir_all(&workspace_root).unwrap();
        fs::create_dir_all(&source).unwrap();
        let mut registry = SandboxRegistry::default();
        registry
            .create_session(config(workspace_root.clone()))
            .unwrap();
        registry
            .register_mount(
                "session-1",
                readonly_mount_with_env(
                    "selected-folder",
                    source,
                    MountScope::Invocation {
                        invocation_id: "inv-1".to_string(),
                    },
                    "COFFICE_SELECTED_FOLDER_DIR",
                ),
            )
            .unwrap();

        let matching = registry
            .session("session-1")
            .unwrap()
            .exec_environment_for(Some(&SandboxInvocationContext {
                session_id: "session-1".to_string(),
                run_id: None,
                scope_id: None,
                invocation_id: Some("inv-1".to_string()),
            }));
        let mismatched = registry
            .session("session-1")
            .unwrap()
            .exec_environment_for(Some(&SandboxInvocationContext {
                session_id: "session-1".to_string(),
                run_id: None,
                scope_id: None,
                invocation_id: Some("inv-2".to_string()),
            }));
        let no_context = registry
            .session("session-1")
            .unwrap()
            .exec_environment_for(None);

        let target = workspace_root
            .canonicalize()
            .unwrap()
            .join("workspace/mounts/selected-folder");
        assert_eq!(
            matching.env["COFFICE_SELECTED_FOLDER_DIR"],
            target.to_string_lossy()
        );
        assert_eq!(matching.mounts.len(), 1);
        assert!(mismatched.mounts.is_empty());
        assert!(!mismatched.env.contains_key("COFFICE_SELECTED_FOLDER_DIR"));
        assert!(no_context.mounts.is_empty());
        assert!(!no_context.env.contains_key("COFFICE_SELECTED_FOLDER_DIR"));
    }

    #[test]
    fn sandbox_exec_environment_inactive_run_mount_env_var_is_omitted_without_context() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        fs::create_dir_all(&workspace_root).unwrap();
        fs::create_dir_all(&source).unwrap();
        let mut registry = SandboxRegistry::default();
        registry.create_session(config(workspace_root)).unwrap();
        registry
            .register_mount(
                "session-1",
                readonly_mount_with_env(
                    "selected-folder",
                    source,
                    MountScope::Run {
                        run_id: "run-1".to_string(),
                    },
                    "COFFICE_SELECTED_FOLDER_DIR",
                ),
            )
            .unwrap();

        let env = registry
            .session("session-1")
            .unwrap()
            .exec_environment_for(None);
        let legacy_env = registry.session("session-1").unwrap().exec_environment();

        assert!(env.mounts.is_empty());
        assert!(!env.env.contains_key("COFFICE_SELECTED_FOLDER_DIR"));
        assert!(legacy_env.mounts.is_empty());
        assert!(!legacy_env.env.contains_key("COFFICE_SELECTED_FOLDER_DIR"));
    }

    #[test]
    fn sandbox_exec_environment_scoped_mount_without_env_var_can_be_inactive() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("sandbox");
        let source = temp.path().join("host");
        fs::create_dir_all(&workspace_root).unwrap();
        fs::create_dir_all(&source).unwrap();
        let mut registry = SandboxRegistry::default();
        registry.create_session(config(workspace_root)).unwrap();
        registry
            .register_mount(
                "session-1",
                readonly_mount(
                    "selected-folder",
                    source,
                    MountScope::Run {
                        run_id: "run-1".to_string(),
                    },
                ),
            )
            .unwrap();

        let env = registry
            .session("session-1")
            .unwrap()
            .exec_environment_for(None);

        assert!(env.mounts.is_empty());
    }
}
