use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct Config {
    /// WebSocket server URL (e.g. "ws://localhost:3000/ws")
    pub server_url: String,

    /// Unique device identifier. Auto-generated if omitted.
    pub device_id: Option<String>,

    /// Maximum number of concurrent jobs. Defaults to 8.
    pub max_concurrent_jobs: Option<usize>,

    /// Directory for trace logs and run artifacts. Defaults to ~/.ahand/data.
    pub data_dir: Option<String>,

    #[serde(default)]
    pub policy: PolicyConfig,
}

#[derive(Debug, Default, Deserialize)]
pub struct PolicyConfig {
    /// If non-empty, only these tools are allowed.
    #[serde(default)]
    pub allowed_tools: Vec<String>,

    /// Working directories that are denied.
    #[serde(default)]
    pub denied_paths: Vec<String>,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn device_id(&self) -> String {
        self.device_id
            .clone()
            .unwrap_or_else(|| uuid_v4())
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
