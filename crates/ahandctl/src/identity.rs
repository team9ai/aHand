use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    hub: Option<HubConfig>,
}

#[derive(Debug, Deserialize)]
struct HubConfig {
    private_key_path: Option<String>,
}

pub fn show(config: Option<String>, identity_path: Option<String>) -> anyhow::Result<()> {
    let path = resolve_identity_path(config.as_deref(), identity_path.as_deref())?;
    let identity = ahandd::DeviceIdentity::load_or_create(&path)
        .with_context(|| format!("failed to load or create identity at {}", path.display()))?;

    let output = json!({
        "deviceId": identity.device_id(),
        "publicKey": identity.public_key_b64(),
        "identityPath": path,
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn resolve_identity_path(
    config: Option<&str>,
    identity_path: Option<&str>,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = identity_path {
        return Ok(expand_home(path));
    }

    if let Some(config_path) = config {
        let content = std::fs::read_to_string(config_path)
            .with_context(|| format!("failed to read config {}", config_path))?;
        let parsed: ConfigFile = toml::from_str(&content)
            .with_context(|| format!("failed to parse config {}", config_path))?;
        if let Some(path) = parsed
            .hub
            .and_then(|hub| hub.private_key_path)
            .filter(|path| !path.trim().is_empty())
        {
            return Ok(expand_home(&path));
        }
    }

    Ok(ahandd::device_identity::default_identity_path())
}

fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    Path::new(path).to_path_buf()
}
