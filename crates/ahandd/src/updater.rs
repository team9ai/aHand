use std::sync::atomic::{AtomicBool, Ordering};

use ahand_protocol::{
    Envelope, UpdateCommand, UpdateState, UpdateStatus, UpdateSuggestion, envelope,
};
use semver::Version;
use tracing::{error, info, warn};

use crate::executor::EnvelopeSink;

/// Embedded Ed25519 public key used to verify release signatures.
const RELEASE_PUB_KEY: &[u8; 32] = include_bytes!("../../../keys/release.pub");

/// Default maximum retry count when the source does not specify one.
const DEFAULT_MAX_RETRIES: u32 = 3;

/// Base delay (in seconds) for exponential backoff between retries.
const RETRY_BASE_DELAY_SECS: u64 = 5;

/// Global flag: only one update may execute at a time.
static UPDATE_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

// ── UpdateParams ───────────────────────────────────────────────────

/// Unified parameters for an update, regardless of whether the update was
/// triggered by an `UpdateSuggestion` (during hello) or an `UpdateCommand`.
#[derive(Debug, Clone)]
pub struct UpdateParams {
    pub update_id: String,
    pub target_version: String,
    pub download_url: String,
    pub checksum_sha256: String,
    pub signature: Vec<u8>,
    pub max_retries: u32,
}

impl From<UpdateSuggestion> for UpdateParams {
    fn from(s: UpdateSuggestion) -> Self {
        Self {
            update_id: s.update_id,
            target_version: s.target_version,
            download_url: s.download_url,
            checksum_sha256: s.checksum_sha256,
            signature: s.signature,
            max_retries: DEFAULT_MAX_RETRIES,
        }
    }
}

impl From<UpdateCommand> for UpdateParams {
    fn from(c: UpdateCommand) -> Self {
        Self {
            update_id: c.update_id,
            target_version: c.target_version,
            download_url: c.download_url,
            checksum_sha256: c.checksum_sha256,
            signature: c.signature,
            max_retries: if c.max_retries == 0 {
                DEFAULT_MAX_RETRIES
            } else {
                c.max_retries
            },
        }
    }
}

// ── Public entry point ─────────────────────────────────────────────

/// Attempt to start an update. Returns `true` if the update task was spawned,
/// `false` if another update is already in progress.
pub fn spawn_update<T: EnvelopeSink>(
    params: UpdateParams,
    device_id: String,
    tx: T,
) -> bool {
    // Downgrade protection: reject if target <= current.
    let current = env!("CARGO_PKG_VERSION");
    let current_ver = match Version::parse(current) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "failed to parse current version — skipping update");
            return false;
        }
    };
    let target_ver = match Version::parse(&params.target_version) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, target = %params.target_version,
                "failed to parse target version — skipping update");
            return false;
        }
    };
    if target_ver <= current_ver {
        warn!(
            current = %current_ver,
            target = %target_ver,
            "target version is not newer than current — skipping update"
        );
        return false;
    }

    // Atomically claim the update slot.
    if UPDATE_IN_PROGRESS
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        warn!("another update is already in progress");
        return false;
    }

    tokio::spawn(async move {
        execute_update(params, device_id, tx).await;
        UPDATE_IN_PROGRESS.store(false, Ordering::SeqCst);
    });

    true
}

// ── Core update logic ──────────────────────────────────────────────

async fn execute_update<T: EnvelopeSink>(
    params: UpdateParams,
    device_id: String,
    tx: T,
) {
    let current_version: String = env!("CARGO_PKG_VERSION").into();
    let mut attempt = 0u32;

    loop {
        attempt += 1;
        info!(
            update_id = %params.update_id,
            attempt,
            max_retries = params.max_retries,
            "starting update attempt"
        );

        match try_update(&params, &device_id, &current_version, &tx).await {
            Ok(()) => return, // success — process will have exec'd itself
            Err(UpdateError::SignatureFailure(msg)) => {
                error!(update_id = %params.update_id, error = %msg,
                    "signature verification failed — aborting (no retry)");
                send_status(
                    &tx,
                    &device_id,
                    &params.update_id,
                    UpdateState::Failed,
                    &current_version,
                    &params.target_version,
                    0,
                    &msg,
                );
                return;
            }
            Err(UpdateError::Retriable(msg)) => {
                warn!(update_id = %params.update_id, attempt, error = %msg, "update attempt failed");
                if attempt >= params.max_retries {
                    error!(update_id = %params.update_id,
                        "all retries exhausted — update failed");
                    send_status(
                        &tx,
                        &device_id,
                        &params.update_id,
                        UpdateState::Failed,
                        &current_version,
                        &params.target_version,
                        0,
                        &format!("failed after {} attempts: {}", attempt, msg),
                    );
                    return;
                }
                // Exponential backoff: 5s, 15s, 45s, …
                let delay = RETRY_BASE_DELAY_SECS * 3u64.pow(attempt - 1);
                info!(delay_secs = delay, "backing off before retry");
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            }
        }
    }
}

enum UpdateError {
    /// Signature verification failed — do NOT retry.
    SignatureFailure(String),
    /// Transient error — eligible for retry.
    Retriable(String),
}

async fn try_update<T: EnvelopeSink>(
    params: &UpdateParams,
    device_id: &str,
    current_version: &str,
    tx: &T,
) -> Result<(), UpdateError> {
    // ── 1. Download ────────────────────────────────────────────
    send_status(
        tx,
        device_id,
        &params.update_id,
        UpdateState::Downloading,
        current_version,
        &params.target_version,
        0,
        "",
    );

    let bytes = download_binary(&params.download_url).await.map_err(|e| {
        UpdateError::Retriable(format!("download failed: {}", e))
    })?;

    // ── 2. Verify checksum ─────────────────────────────────────
    send_status(
        tx,
        device_id,
        &params.update_id,
        UpdateState::Verifying,
        current_version,
        &params.target_version,
        50,
        "",
    );

    if !params.checksum_sha256.is_empty() {
        verify_checksum(&bytes, &params.checksum_sha256).map_err(|e| {
            UpdateError::Retriable(format!("checksum mismatch: {}", e))
        })?;
    }

    // ── 3. Verify signature ────────────────────────────────────
    if !params.signature.is_empty() {
        verify_signature(&bytes, &params.signature).map_err(|e| {
            UpdateError::SignatureFailure(format!("signature verification failed: {}", e))
        })?;
    }

    // ── 4. Install ─────────────────────────────────────────────
    send_status(
        tx,
        device_id,
        &params.update_id,
        UpdateState::Installing,
        current_version,
        &params.target_version,
        75,
        "",
    );

    install_binary(&bytes, &params.target_version).map_err(|e| {
        UpdateError::Retriable(format!("installation failed: {}", e))
    })?;

    // ── 5. Restart ─────────────────────────────────────────────
    send_status(
        tx,
        device_id,
        &params.update_id,
        UpdateState::Restarting,
        current_version,
        &params.target_version,
        100,
        "",
    );

    restart_daemon().map_err(|e| {
        UpdateError::Retriable(format!("restart failed: {}", e))
    })?;

    // If exec() succeeded we never reach here; this is a fallback.
    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────

async fn download_binary(url: &str) -> anyhow::Result<Vec<u8>> {
    let resp = reqwest::get(url).await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {}", status);
    }
    let bytes = resp.bytes().await?;
    info!(size = bytes.len(), "downloaded update binary");
    Ok(bytes.to_vec())
}

fn verify_checksum(data: &[u8], expected_hex: &str) -> anyhow::Result<()> {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(data);
    let actual_hex = hex::encode(digest);
    if actual_hex != expected_hex {
        anyhow::bail!(
            "expected {}, got {}",
            expected_hex,
            actual_hex
        );
    }
    Ok(())
}

fn verify_signature(data: &[u8], signature_bytes: &[u8]) -> anyhow::Result<()> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    let pubkey = VerifyingKey::from_bytes(RELEASE_PUB_KEY)
        .map_err(|e| anyhow::anyhow!("invalid embedded public key: {}", e))?;

    let sig = Signature::try_from(signature_bytes)
        .map_err(|e| anyhow::anyhow!("invalid signature bytes: {}", e))?;

    pubkey
        .verify(data, &sig)
        .map_err(|e| anyhow::anyhow!("ed25519 verification failed: {}", e))?;

    info!("release signature verified successfully");
    Ok(())
}

fn install_binary(data: &[u8], target_version: &str) -> anyhow::Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let bin_dir = home.join(".ahand").join("bin");
    std::fs::create_dir_all(&bin_dir)?;

    let target_path = bin_dir.join("ahandd");
    let tmp_path = bin_dir.join("ahandd.update.tmp");

    // Write to temp file, then atomically rename.
    std::fs::write(&tmp_path, data)?;

    // Make executable (Unix).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&tmp_path, perms)?;
    }

    std::fs::rename(&tmp_path, &target_path)?;
    info!(path = %target_path.display(), "installed new binary");

    // Write version marker.
    let version_path = home.join(".ahand").join("version");
    std::fs::write(&version_path, target_version)?;
    info!(version = %target_version, "wrote version marker");

    Ok(())
}

fn restart_daemon() -> anyhow::Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let bin_path = home.join(".ahand").join("bin").join("ahandd");

    info!(path = %bin_path.display(), "exec()-ing new daemon binary");

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Collect current args (skip argv[0]).
        let args: Vec<String> = std::env::args().skip(1).collect();
        let err = std::process::Command::new(&bin_path).args(&args).exec();
        // exec() only returns on error.
        anyhow::bail!("exec() failed: {}", err);
    }

    #[cfg(not(unix))]
    {
        anyhow::bail!("restart via exec() is only supported on Unix");
    }
}

fn send_status<T: EnvelopeSink>(
    tx: &T,
    device_id: &str,
    update_id: &str,
    state: UpdateState,
    current_version: &str,
    target_version: &str,
    progress: u32,
    error_msg: &str,
) {
    let envelope = Envelope {
        device_id: device_id.to_string(),
        msg_id: format!("update-{}-{}", update_id, state.as_str_name()),
        payload: Some(envelope::Payload::UpdateStatus(UpdateStatus {
            update_id: update_id.to_string(),
            state: state as i32,
            current_version: current_version.to_string(),
            target_version: target_version.to_string(),
            progress,
            error: error_msg.to_string(),
        })),
        ..Default::default()
    };
    if tx.send(envelope).is_err() {
        warn!("failed to send update status — channel closed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_params_from_suggestion_defaults_max_retries() {
        let suggestion = UpdateSuggestion {
            update_id: "u-1".into(),
            target_version: "1.2.3".into(),
            download_url: "https://example.com/bin".into(),
            checksum_sha256: "abc123".into(),
            signature: vec![1, 2, 3],
            release_notes: "notes".into(),
        };
        let params = UpdateParams::from(suggestion);
        assert_eq!(params.update_id, "u-1");
        assert_eq!(params.target_version, "1.2.3");
        assert_eq!(params.download_url, "https://example.com/bin");
        assert_eq!(params.checksum_sha256, "abc123");
        assert_eq!(params.signature, vec![1, 2, 3]);
        assert_eq!(params.max_retries, DEFAULT_MAX_RETRIES);
    }

    #[test]
    fn update_params_from_command_uses_specified_retries() {
        let cmd = UpdateCommand {
            update_id: "u-2".into(),
            target_version: "2.0.0".into(),
            download_url: "https://example.com/bin2".into(),
            checksum_sha256: "def456".into(),
            signature: vec![4, 5, 6],
            max_retries: 5,
        };
        let params = UpdateParams::from(cmd);
        assert_eq!(params.update_id, "u-2");
        assert_eq!(params.max_retries, 5);
    }

    #[test]
    fn update_params_from_command_zero_retries_uses_default() {
        let cmd = UpdateCommand {
            update_id: "u-3".into(),
            target_version: "3.0.0".into(),
            download_url: "https://example.com/bin3".into(),
            checksum_sha256: "".into(),
            signature: vec![],
            max_retries: 0,
        };
        let params = UpdateParams::from(cmd);
        assert_eq!(params.max_retries, DEFAULT_MAX_RETRIES);
    }
}
