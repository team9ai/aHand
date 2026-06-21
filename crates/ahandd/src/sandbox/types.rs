use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const CODE_SANDBOX_UNAVAILABLE: &str = "SANDBOX_UNAVAILABLE";
pub const CODE_PERMISSION_DENIED: &str = "PERMISSION_DENIED";
pub const CODE_INVALID_SANDBOX_PATH: &str = "INVALID_SANDBOX_PATH";
pub const CODE_UNKNOWN_FILE_REF: &str = "UNKNOWN_FILE_REF";
pub const CODE_UNKNOWN_VERSION: &str = "UNKNOWN_VERSION";
pub const CODE_RUNTIME_NOT_REGISTERED: &str = "RUNTIME_NOT_REGISTERED";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxPermissionMode {
    Readonly,
    Copy,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkPolicy {
    Enabled,
    Disabled,
    ProxyOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionSnapshot {
    pub mode: SandboxPermissionMode,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeProviderConfig {
    pub name: String,
    pub executable: PathBuf,
    pub readonly_roots: Vec<PathBuf>,
    pub env: HashMap<String, String>,
    pub default_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxSessionConfig {
    pub session_id: String,
    pub permission_mode: SandboxPermissionMode,
    pub workspace_root: PathBuf,
    pub network: NetworkPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFileRef {
    pub file_ref_id: String,
    pub source_path: PathBuf,
    pub display_name: String,
    pub size: u64,
    pub mtime_ms: Option<u128>,
    pub conversation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxFile {
    pub sandbox_file_id: String,
    pub file_ref_id: String,
    pub sandbox_path: PathBuf,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeExecuteRequest {
    pub runtime: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: HashMap<String, String>,
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeExecuteResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterVersionRequest {
    pub sandbox_path: PathBuf,
    pub source_file_ref_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileVersion {
    pub version_id: String,
    pub sandbox_path: PathBuf,
    pub source_file_ref_id: Option<String>,
    pub size: u64,
    pub hash: String,
    pub status: FileVersionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileVersionStatus {
    Candidate,
    Committed,
    Rejected,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitResult {
    pub version_id: String,
    pub source_file_ref_id: String,
    pub backup_id: Option<String>,
    pub old_hash: Option<String>,
    pub new_hash: String,
    pub bytes_written: u64,
    pub permission_mode: SandboxPermissionMode,
    pub permission_version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxError {
    pub code: String,
    pub message: String,
}

pub type SandboxResult<T> = Result<T, SandboxError>;

impl SandboxError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(CODE_SANDBOX_UNAVAILABLE, message)
    }

    pub fn permission_denied(message: impl Into<String>) -> Self {
        Self::new(CODE_PERMISSION_DENIED, message)
    }

    pub fn invalid_sandbox_path(message: impl Into<String>) -> Self {
        Self::new(CODE_INVALID_SANDBOX_PATH, message)
    }

    pub fn runtime_not_registered(message: impl Into<String>) -> Self {
        Self::new(CODE_RUNTIME_NOT_REGISTERED, message)
    }

    pub fn unknown_file_ref(message: impl Into<String>) -> Self {
        Self::new(CODE_UNKNOWN_FILE_REF, message)
    }

    pub fn unknown_version(message: impl Into<String>) -> Self {
        Self::new(CODE_UNKNOWN_VERSION, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn permission_and_network_modes_serialize_lowercase() {
        assert_eq!(
            serde_json::to_value(SandboxPermissionMode::Readonly).unwrap(),
            json!("readonly")
        );
        assert_eq!(
            serde_json::to_value(SandboxPermissionMode::Copy).unwrap(),
            json!("copy")
        );
        assert_eq!(
            serde_json::to_value(SandboxPermissionMode::Full).unwrap(),
            json!("full")
        );
        assert_eq!(
            serde_json::to_value(NetworkPolicy::Enabled).unwrap(),
            json!("enabled")
        );
    }

    #[test]
    fn runtime_provider_keeps_executable_roots_env_and_timeout() {
        let provider = RuntimeProviderConfig {
            name: "python".to_string(),
            executable: PathBuf::from("/opt/coffice/python/bin/python"),
            readonly_roots: vec![PathBuf::from("/opt/coffice/python")],
            env: HashMap::from([(
                "PYTHONPATH".to_string(),
                "/opt/coffice/python/lib".to_string(),
            )]),
            default_timeout: Duration::from_secs(30),
        };

        assert_eq!(provider.name, "python");
        assert_eq!(
            provider.executable,
            PathBuf::from("/opt/coffice/python/bin/python")
        );
        assert_eq!(
            provider.readonly_roots,
            vec![PathBuf::from("/opt/coffice/python")]
        );
        assert_eq!(provider.env["PYTHONPATH"], "/opt/coffice/python/lib");
        assert_eq!(provider.default_timeout, Duration::from_secs(30));
    }

    #[test]
    fn sandbox_error_preserves_code_and_message() {
        let err = SandboxError::permission_denied("full permission is required");

        assert_eq!(err.code, "PERMISSION_DENIED");
        assert_eq!(err.message, "full permission is required");
    }
}
