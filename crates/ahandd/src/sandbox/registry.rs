use std::collections::BTreeMap;
use std::path::PathBuf;

use super::types::{
    FileVersion, HostFileRef, NetworkPolicy, PermissionSnapshot, RuntimeProviderConfig,
    SandboxError, SandboxFile, SandboxPermissionMode, SandboxResult, SandboxSessionConfig,
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
}

#[derive(Debug, Default)]
pub struct SandboxRegistry {
    sessions: BTreeMap<String, SandboxSessionState>,
}

impl SandboxRegistry {
    pub fn create_session(&mut self, config: SandboxSessionConfig) -> SandboxResult<()> {
        let session_id = config.session_id.clone();
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
    use std::path::PathBuf;

    fn config() -> SandboxSessionConfig {
        SandboxSessionConfig {
            session_id: "session-1".to_string(),
            permission_mode: SandboxPermissionMode::Readonly,
            workspace_root: PathBuf::from("/tmp/coffice/session-1"),
            network: NetworkPolicy::Enabled,
        }
    }

    #[test]
    fn create_session_initializes_permission_version() {
        let mut registry = SandboxRegistry::default();

        registry.create_session(config()).unwrap();
        let snapshot = registry.permission_snapshot("session-1").unwrap();

        assert_eq!(snapshot.mode, SandboxPermissionMode::Readonly);
        assert_eq!(snapshot.version, 1);
    }

    #[test]
    fn permission_update_increments_only_on_change() {
        let mut registry = SandboxRegistry::default();
        registry.create_session(config()).unwrap();

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
}
