use std::path::{Path, PathBuf};
use std::{fs::OpenOptions, io::Write};

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

const IDENTITY_FILE: &str = "hub-device-identity.json";

#[derive(Debug, Clone)]
pub struct DeviceIdentity {
    signing_key: SigningKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredIdentity {
    version: u32,
    #[serde(rename = "privateKeyBase64")]
    private_key_base64: String,
}

impl DeviceIdentity {
    pub fn generate() -> Self {
        Self {
            signing_key: SigningKey::generate(&mut OsRng),
        }
    }

    #[allow(dead_code)]
    pub fn generate_for_tests() -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&[7u8; 32]),
        }
    }

    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            return Self::load(path);
        }

        let identity = Self::generate();
        identity.save(path)?;
        Ok(identity)
    }

    pub fn public_key_bytes(&self) -> Vec<u8> {
        self.signing_key.verifying_key().to_bytes().to_vec()
    }

    pub fn sign_hello(
        &self,
        device_id: &str,
        signed_at_ms: u64,
        challenge_nonce: &[u8],
    ) -> Vec<u8> {
        let payload =
            ahand_protocol::build_hello_auth_payload(device_id, signed_at_ms, challenge_nonce);
        self.signing_key.sign(&payload).to_bytes().to_vec()
    }

    #[allow(dead_code)]
    pub fn to_bootstrap_header(&self) -> String {
        STANDARD.encode(self.public_key_bytes())
    }

    fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let stored: StoredIdentity = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))?;

        if stored.version != 1 {
            anyhow::bail!("unsupported identity version: {}", stored.version);
        }

        let secret_bytes = STANDARD
            .decode(&stored.private_key_base64)
            .context("failed to decode hub private key")?;
        let secret_len = secret_bytes.len();
        let secret: [u8; 32] = secret_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid hub private key length: {}", secret_len))?;

        Ok(Self {
            signing_key: SigningKey::from_bytes(&secret),
        })
    }

    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create hub identity directory {}",
                    parent.display()
                )
            })?;
        }

        let stored = StoredIdentity {
            version: 1,
            private_key_base64: STANDARD.encode(self.signing_key.to_bytes()),
        };

        let content =
            serde_json::to_string_pretty(&stored).context("failed to serialize hub identity")?;
        write_secure_file(path, format!("{content}\n").as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;

        Ok(())
    }
}

pub fn default_identity_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ahand")
        .join(IDENTITY_FILE)
}

fn write_secure_file(path: &Path, content: &[u8]) -> Result<()> {
    let tmp_path = path.with_extension(format!(
        "tmp-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&tmp_path)?;
        file.write_all(content)?;
        file.sync_all()?;
    }

    #[cfg(not(unix))]
    {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)?;
        file.write_all(content)?;
        file.sync_all()?;
    }

    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::write_secure_file;

    #[cfg(unix)]
    #[test]
    fn write_secure_file_uses_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "ahandd-device-identity-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("identity.json");

        write_secure_file(&path, b"secret").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
