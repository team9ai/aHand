use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Connection mode for ahandd
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConnectionMode {
    /// Connect to aHand Cloud (default)
    #[default]
    AHandCloud,
    /// Connect to OpenClaw Gateway as a node
    OpenClawGateway,
}

impl ConnectionMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "openclaw-gateway" | "openclaw" => Self::OpenClawGateway,
            _ => Self::AHandCloud,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    /// Connection mode: "ahand-cloud" (default) or "openclaw-gateway"
    #[serde(default)]
    pub mode: Option<String>,

    /// WebSocket server URL (e.g. "ws://localhost:3000/ws") - for ahand-cloud mode
    #[serde(default = "default_server_url")]
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

    /// OpenClaw Gateway configuration (when mode = "openclaw-gateway")
    #[serde(default)]
    pub openclaw: Option<OpenClawConfig>,
}

/// OpenClaw Gateway connection configuration
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct OpenClawConfig {
    /// Gateway host (e.g., "127.0.0.1")
    pub gateway_host: Option<String>,

    /// Gateway port (default: 18789)
    pub gateway_port: Option<u16>,

    /// Use TLS (wss://)
    #[serde(default)]
    pub gateway_tls: Option<bool>,

    /// TLS certificate fingerprint for pinning
    pub gateway_tls_fingerprint: Option<String>,

    /// Node ID (auto-generated if not set)
    pub node_id: Option<String>,

    /// Display name for this node
    pub display_name: Option<String>,

    /// Authentication token
    pub auth_token: Option<String>,

    /// Authentication password
    pub auth_password: Option<String>,

    /// Path to exec-approvals.json
    pub exec_approvals_path: Option<String>,
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

fn default_server_url() -> String {
    "ws://localhost:3000/ws".to_string()
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    /// Get the connection mode
    pub fn connection_mode(&self) -> ConnectionMode {
        self.mode
            .as_ref()
            .map(|s| ConnectionMode::from_str(s))
            .unwrap_or_default()
    }

    /// Get OpenClaw config, creating default if needed
    pub fn openclaw_config(&self) -> OpenClawConfig {
        self.openclaw.clone().unwrap_or_default()
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
