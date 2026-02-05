//! Device pairing for OpenClaw Gateway.
//!
//! Manages node registration and authentication with the Gateway.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const PAIRING_FILE: &str = "openclaw-pairing.json";

/// Pairing state stored locally
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PairingState {
    /// Node ID
    #[serde(rename = "nodeId")]
    pub node_id: String,

    /// Pairing token (received from Gateway after approval)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,

    /// Display name
    #[serde(rename = "displayName", skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,

    /// Gateway connection info
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway: Option<GatewayInfo>,

    /// Timestamp when paired
    #[serde(rename = "pairedAt", skip_serializing_if = "Option::is_none")]
    pub paired_at: Option<u64>,
}

/// Gateway connection info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayInfo {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub tls: bool,
    #[serde(rename = "tlsFingerprint", skip_serializing_if = "Option::is_none")]
    pub tls_fingerprint: Option<String>,
}

/// Get the default pairing state file path
pub fn default_pairing_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ahand")
        .join(PAIRING_FILE)
}

/// Load pairing state from file
pub fn load_pairing_state(path: &PathBuf) -> Result<Option<PairingState>> {
    if !path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let state: PairingState = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;

    Ok(Some(state))
}

/// Save pairing state to file
pub fn save_pairing_state(path: &PathBuf, state: &PairingState) -> Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    let content = serde_json::to_string_pretty(state)
        .context("failed to serialize pairing state")?;

    std::fs::write(path, format!("{}\n", content))
        .with_context(|| format!("failed to write {}", path.display()))?;

    // Set file permissions to 0600 (user read/write only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(path, perms);
    }

    Ok(())
}

/// Generate a new node ID if not provided
pub fn generate_node_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
