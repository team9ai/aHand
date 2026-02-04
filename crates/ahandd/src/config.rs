use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    /// WebSocket server URL (e.g. "ws://localhost:3000/ws")
    pub server_url: String,

    /// Unique device identifier. Auto-generated if omitted.
    pub device_id: Option<String>,

    /// Maximum number of concurrent jobs. Defaults to 8.
    pub max_concurrent_jobs: Option<usize>,

    /// Directory for trace logs and run artifacts. Defaults to ~/.ahand/data.
    pub data_dir: Option<String>,

    /// Enable debug IPC server (Unix socket).
    #[serde(default)]
    pub debug_ipc: Option<bool>,

    /// Custom path for the IPC Unix socket. Defaults to ~/.ahand/ahandd.sock.
    pub ipc_socket_path: Option<String>,

    /// Unix permission mode for the IPC socket (e.g. 0o660 for group access).
    /// Defaults to 0o660.
    pub ipc_socket_mode: Option<u32>,

    /// Default trust timeout in minutes for Trust mode. Defaults to 60.
    pub trust_timeout_mins: Option<u64>,

    #[serde(default)]
    pub policy: PolicyConfig,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PolicyConfig {
    /// If non-empty, only these tools are allowed without approval.
    #[serde(default)]
    pub allowed_tools: Vec<String>,

    /// Working directories that are denied (hard reject, no approval).
    #[serde(default)]
    pub denied_paths: Vec<String>,

    /// Tools that are always denied (hard reject, no approval opportunity).
    #[serde(default)]
    pub denied_tools: Vec<String>,

    /// Domains that are allowed without approval for network tools.
    #[serde(default)]
    pub allowed_domains: Vec<String>,

    /// How long to wait for user approval before rejecting (seconds).
    /// Defaults to 86400 (24 hours).
    #[serde(default = "default_approval_timeout")]
    pub approval_timeout_secs: u64,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            allowed_tools: Vec::new(),
            denied_paths: Vec::new(),
            denied_tools: Vec::new(),
            allowed_domains: Vec::new(),
            approval_timeout_secs: default_approval_timeout(),
        }
    }
}

fn default_approval_timeout() -> u64 {
    86400
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    /// Serialize and write the config back to a TOML file.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn device_id(&self) -> String {
        self.device_id
            .clone()
            .unwrap_or_else(uuid_v4)
    }

    /// Resolve the IPC socket path. Default: ~/.ahand/ahandd.sock.
    pub fn ipc_socket_path(&self) -> PathBuf {
        match &self.ipc_socket_path {
            Some(p) => PathBuf::from(p),
            None => dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".ahand")
                .join("ahandd.sock"),
        }
    }

    /// Get the IPC socket permission mode. Default: 0o660.
    pub fn ipc_socket_mode(&self) -> u32 {
        self.ipc_socket_mode.unwrap_or(0o660)
    }

    /// Resolve the data directory path. Returns `None` only if explicitly
    /// set to an empty string (indicating the user wants persistence disabled).
    pub fn data_dir(&self) -> Option<PathBuf> {
        match &self.data_dir {
            Some(dir) if dir.is_empty() => None,
            Some(dir) => Some(PathBuf::from(dir)),
            None => {
                // Default: ~/.ahand/data
                dirs::home_dir().map(|h| h.join(".ahand").join("data"))
            }
        }
    }
}

fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:032x}", ts)
}
