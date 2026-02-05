//! Device identity for OpenClaw Gateway authentication.
//!
//! Generates and manages Ed25519 keypairs for device authentication.

use std::path::PathBuf;

use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ed25519_dalek::{SecretKey, SigningKey, VerifyingKey, Signer};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const IDENTITY_FILE: &str = "device-identity.json";

/// Device identity with Ed25519 keypair
#[derive(Debug, Clone)]
pub struct DeviceIdentity {
    pub device_id: String,
    pub signing_key: SigningKey,
    pub verifying_key: VerifyingKey,
}

/// Stored identity format
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredIdentity {
    version: u32,
    #[serde(rename = "deviceId")]
    device_id: String,
    #[serde(rename = "privateKeyBase64")]
    private_key_base64: String,
    #[serde(rename = "createdAtMs")]
    created_at_ms: u64,
}

impl DeviceIdentity {
    /// Generate a new device identity
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        let device_id = derive_device_id(&verifying_key);

        Self {
            device_id,
            signing_key,
            verifying_key,
        }
    }

    /// Load from stored format or generate new
    pub fn load_or_create(path: &PathBuf) -> Result<Self> {
        if path.exists() {
            match Self::load(path) {
                Ok(identity) => return Ok(identity),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load device identity, regenerating");
                }
            }
        }

        let identity = Self::generate();
        identity.save(path)?;
        Ok(identity)
    }

    /// Load from file
    fn load(path: &PathBuf) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;

        let stored: StoredIdentity = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))?;

        if stored.version != 1 {
            anyhow::bail!("unsupported identity version: {}", stored.version);
        }

        let private_key_bytes = URL_SAFE_NO_PAD
            .decode(&stored.private_key_base64)
            .context("failed to decode private key")?;

        if private_key_bytes.len() != 32 {
            anyhow::bail!("invalid private key length: {}", private_key_bytes.len());
        }

        let secret_key: SecretKey = private_key_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid private key"))?;
        let signing_key = SigningKey::from_bytes(&secret_key);
        let verifying_key = signing_key.verifying_key();
        let device_id = derive_device_id(&verifying_key);

        // Verify device ID matches (or update if different)
        if device_id != stored.device_id {
            tracing::warn!(
                stored = %stored.device_id,
                derived = %device_id,
                "device ID mismatch, using derived"
            );
        }

        Ok(Self {
            device_id,
            signing_key,
            verifying_key,
        })
    }

    /// Save to file
    fn save(&self, path: &PathBuf) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }

        let stored = StoredIdentity {
            version: 1,
            device_id: self.device_id.clone(),
            private_key_base64: URL_SAFE_NO_PAD.encode(self.signing_key.to_bytes()),
            created_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        };

        let content = serde_json::to_string_pretty(&stored)
            .context("failed to serialize identity")?;

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

    /// Get the raw public key bytes (32 bytes for Ed25519)
    pub fn public_key_raw(&self) -> [u8; 32] {
        self.verifying_key.to_bytes()
    }

    /// Get the public key as base64url (no padding)
    pub fn public_key_base64url(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.public_key_raw())
    }

    /// Sign a payload and return base64url signature
    pub fn sign(&self, payload: &str) -> String {
        let signature = self.signing_key.sign(payload.as_bytes());
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    }
}

/// Derive device ID from public key (SHA256 hash of raw public key)
fn derive_device_id(verifying_key: &VerifyingKey) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifying_key.to_bytes());
    let hash = hasher.finalize();
    hex::encode(hash)
}

/// Get the default device identity file path
pub fn default_identity_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ahand")
        .join(IDENTITY_FILE)
}

/// Build the auth payload for signing
pub fn build_auth_payload(
    device_id: &str,
    client_id: &str,
    client_mode: &str,
    role: &str,
    scopes: &[String],
    signed_at_ms: u64,
    token: Option<&str>,
    nonce: Option<&str>,
) -> String {
    let scopes_str = scopes.join(",");
    let token_str = token.unwrap_or("");
    let signed_at_str = signed_at_ms.to_string();

    // Use v2 if nonce is present, otherwise v1
    let version = if nonce.is_some() { "v2" } else { "v1" };

    let mut parts = vec![
        version,
        device_id,
        client_id,
        client_mode,
        role,
        &scopes_str,
        &signed_at_str,
        token_str,
    ];

    if version == "v2" {
        parts.push(nonce.unwrap_or(""));
    }

    parts.join("|")
}

// Add hex encoding since we don't have a hex crate
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes
            .as_ref()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    }
}
