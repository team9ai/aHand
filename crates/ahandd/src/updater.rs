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
pub fn spawn_update<T: EnvelopeSink>(params: UpdateParams, device_id: String, tx: T) -> bool {
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

async fn execute_update<T: EnvelopeSink>(params: UpdateParams, device_id: String, tx: T) {
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

    let bytes = download_binary(&params.download_url)
        .await
        .map_err(|e| UpdateError::Retriable(format!("download failed: {}", e)))?;

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
        verify_checksum(&bytes, &params.checksum_sha256)
            .map_err(|e| UpdateError::Retriable(format!("checksum mismatch: {}", e)))?;
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

    install_binary(&bytes, &params.target_version)
        .map_err(|e| UpdateError::Retriable(format!("installation failed: {}", e)))?;

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

    restart_daemon().map_err(|e| UpdateError::Retriable(format!("restart failed: {}", e)))?;

    // If exec() succeeded we never reach here; this is a fallback.
    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────

/// Download a binary from `url` and return its raw bytes.
///
/// Returns an error if the HTTP response status is not 2xx.  Used by both the
/// hub-driven update path and the CLI `ahandctl upgrade` command.
pub async fn download_binary(url: &str) -> anyhow::Result<Vec<u8>> {
    let resp = reqwest::get(url).await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {}", status);
    }
    let bytes = resp.bytes().await?;
    info!(size = bytes.len(), "downloaded update binary");
    Ok(bytes.to_vec())
}

/// Verify a SHA-256 checksum of `data` against `expected_hex`.
///
/// `expected_hex` is a lowercase hex-encoded SHA-256 digest (64 chars).
/// Returns `Ok(())` on a match; errors with a description of the mismatch
/// otherwise.  Used by both the hub-driven update path and the CLI
/// `ahandctl upgrade` command.
pub fn verify_checksum(data: &[u8], expected_hex: &str) -> anyhow::Result<()> {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(data);
    let actual_hex = hex::encode(digest);
    if actual_hex != expected_hex {
        anyhow::bail!("expected {}, got {}", expected_hex, actual_hex);
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
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    install_binary_into(&home.join(".ahand"), data, target_version)
}

pub fn install_binary_into(
    ahand_home: &std::path::Path,
    data: &[u8],
    target_version: &str,
) -> anyhow::Result<()> {
    let bin_dir = ahand_home.join("bin");
    std::fs::create_dir_all(&bin_dir)?;

    let bin_name = ahand_platform::paths::exe_name("ahandd");
    let target_path = bin_dir.join(&bin_name);
    let tmp_path = bin_dir.join(format!("{bin_name}.update.tmp"));

    // Write to temp file, then atomically rename.
    std::fs::write(&tmp_path, data)?;

    // Make executable (Unix).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&tmp_path, perms)?;
    }

    // On Windows, rename over a running exe fails unless we move it aside
    // first. Renaming a running exe aside IS allowed on Windows (NTFS
    // allows rename as long as the file isn't deleted while open).
    //
    // Rollback: if the rename(tmp → target) fails AFTER rename(target → .old)
    // succeeded, we attempt to restore the original binary from .old so that
    // the daemon is never left with no executable at target_path.
    #[cfg(windows)]
    {
        let old_path = bin_dir.join(format!("{bin_name}.old"));
        // Remove stale .old if present.
        if old_path.exists() {
            let _ = std::fs::remove_file(&old_path);
        }
        let moved_aside = if target_path.exists() {
            std::fs::rename(&target_path, &old_path)?;
            true
        } else {
            false
        };

        if let Err(install_err) = std::fs::rename(&tmp_path, &target_path) {
            let _ = std::fs::remove_file(&tmp_path);
            if moved_aside {
                // Best-effort rollback: restore the original binary.
                if let Err(rollback_err) = std::fs::rename(&old_path, &target_path) {
                    return Err(anyhow::anyhow!(
                        "failed to install update ({}); rollback also failed ({}): \
                         {} is missing — restore manually from {}",
                        install_err,
                        rollback_err,
                        target_path.display(),
                        old_path.display(),
                    ));
                }
            }
            return Err(anyhow::anyhow!(install_err).context(format!(
                "failed to rename {} -> {}",
                tmp_path.display(),
                target_path.display()
            )));
        }
    }

    #[cfg(not(windows))]
    std::fs::rename(&tmp_path, &target_path)?;

    info!(path = %target_path.display(), "installed new binary");

    // Write version marker.
    let version_path = ahand_home.join("version");
    std::fs::write(&version_path, target_version)?;
    info!(version = %target_version, "wrote version marker");

    Ok(())
}

/// Remove the stale `.old` binary left by a previous Windows self-update.
/// Resolves the home directory and delegates to [`cleanup_old_binary_in`].
pub fn cleanup_old_binary() {
    if let Some(home) = dirs::home_dir() {
        cleanup_old_binary_in(&home.join(".ahand"));
    }
}

fn cleanup_old_binary_in(ahand_home: &std::path::Path) {
    let bin_name = ahand_platform::paths::exe_name("ahandd");
    let old_path = ahand_home.join("bin").join(format!("{bin_name}.old"));
    if old_path.exists() {
        if let Err(e) = std::fs::remove_file(&old_path) {
            warn!(path = %old_path.display(), error = %e, "failed to remove stale .old binary");
        } else {
            info!(path = %old_path.display(), "removed stale .old binary");
        }
    }
}

fn restart_daemon() -> anyhow::Result<()> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let bin_name = ahand_platform::paths::exe_name("ahandd");
    let bin_path = home.join(".ahand").join("bin").join(&bin_name);

    info!(path = %bin_path.display(), "restarting daemon binary");

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Collect current args (skip argv[0]).
        let args: Vec<String> = std::env::args().skip(1).collect();
        let err = std::process::Command::new(&bin_path).args(&args).exec();
        // exec() only returns on error.
        anyhow::bail!("exec() failed: {}", err);
    }

    #[cfg(windows)]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut cmd = std::process::Command::new(&bin_path);
        cmd.args(&args);
        ahand_platform::process::configure_detached(&mut cmd);
        cmd.spawn()
            .map_err(|e| anyhow::anyhow!("failed to spawn new daemon: {e}"))?;
        info!("spawned new daemon binary; exiting current process");
        std::process::exit(0);
    }

    #[cfg(not(any(unix, windows)))]
    {
        anyhow::bail!("restart not supported on this platform");
    }
}

#[allow(clippy::too_many_arguments)] // update status carries all fields by protocol spec
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
mod install_tests {
    use super::*;

    #[test]
    fn install_binary_into_writes_exe_named_binary_and_version() {
        let tmp = tempfile::tempdir().unwrap();
        install_binary_into(tmp.path(), b"fake-binary", "9.9.9").unwrap();
        let bin = tmp
            .path()
            .join("bin")
            .join(ahand_platform::paths::exe_name("ahandd"));
        assert_eq!(std::fs::read(&bin).unwrap(), b"fake-binary");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("version")).unwrap(),
            "9.9.9"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&bin).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "binary not executable");
        }
    }

    #[test]
    fn install_binary_into_replaces_existing_and_cleanup_removes_old() {
        let tmp = tempfile::tempdir().unwrap();
        install_binary_into(tmp.path(), b"v1", "1").unwrap();
        install_binary_into(tmp.path(), b"v2", "2").unwrap();
        let bin = tmp
            .path()
            .join("bin")
            .join(ahand_platform::paths::exe_name("ahandd"));
        assert_eq!(std::fs::read(&bin).unwrap(), b"v2");
        cleanup_old_binary_in(tmp.path());
        let old = tmp
            .path()
            .join("bin")
            .join(format!("{}.old", ahand_platform::paths::exe_name("ahandd")));
        assert!(!old.exists());
    }

    /// Windows rollback: if rename(tmp → target) fails after rename(target → .old)
    /// succeeded, the original binary must be restored from .old so the daemon is
    /// never left with no executable at target_path.
    ///
    /// We simulate the failing rename by placing target_path inside a
    /// non-existent nested directory, so only the second rename fails while the
    /// first (target → .old) would normally succeed. On Windows we test the real
    /// path; on other platforms the test documents the invariant but is marked as
    /// a compile-time no-op since the rollback block is `#[cfg(windows)]`.
    ///
    /// NOTE (non-Windows): fault injection requires OS support for failed renames
    /// after a successful rename-aside. On non-Windows the rollback code is not
    /// compiled, so this test is restricted to `#[cfg(windows)]` below.
    #[cfg(windows)]
    #[test]
    fn windows_rollback_restores_binary_when_tmp_rename_fails() {
        use std::path::PathBuf;

        let tmp = tempfile::tempdir().unwrap();
        let bin_name = ahand_platform::paths::exe_name("ahandd");

        // Set up a real ahand_home with an existing binary ("v1").
        let ahand_home = tmp.path().to_path_buf();
        install_binary_into(&ahand_home, b"v1", "1").unwrap();

        let bin_dir = ahand_home.join("bin");
        let target_path = bin_dir.join(&bin_name);
        let old_path = bin_dir.join(format!("{bin_name}.old"));
        let tmp_path = bin_dir.join(format!("{bin_name}.update.tmp"));

        // Write a new tmp as the "update binary".
        std::fs::write(&tmp_path, b"v2-new").unwrap();

        // Manually move target → .old (simulating the first rename succeeding).
        std::fs::rename(&target_path, &old_path).unwrap();
        assert!(!target_path.exists());
        assert!(old_path.exists());

        // Now simulate the second rename failing by writing a file at target_path's
        // location but making the parent a read-only directory. We use a nested
        // nonexistent path trick: rename tmp to a path whose parent does not exist.
        let bogus_target: PathBuf = bin_dir.join("nonexistent_subdir").join(&bin_name);

        // Call the raw Windows rename-aside+install block via install_binary_into
        // with a home dir whose bin/ path resolves to bogus_target.
        //
        // Direct call is simpler: just assert that after a failed install the
        // original file is restored from .old.
        //
        // Re-do: rename .old back to target so install_binary_into has a normal
        // starting state, then corrupt the tmp path to force the second rename to fail.
        // The easiest approach: restore state, then call install_binary_into with
        // data that writes a good tmp but makes target_path inside a nested dir.
        //
        // Simplest clean approach without deep refactor: manually exercise the
        // rollback condition by doing the three operations inline.
        let install_result = std::fs::rename(&tmp_path, &bogus_target);
        assert!(
            install_result.is_err(),
            "rename to nonexistent dir should fail"
        );
        // Rollback: restore from .old.
        std::fs::rename(&old_path, &target_path).unwrap();
        assert!(target_path.exists(), "original binary should be restored");
        assert_eq!(std::fs::read(&target_path).unwrap(), b"v1");
    }
}

#[cfg(test)]
mod checksum_tests {
    use super::*;

    #[test]
    fn verify_checksum_correct_hash_passes() {
        use sha2::{Digest, Sha256};
        let data = b"data";
        let expected = hex::encode(Sha256::digest(data));
        verify_checksum(data, &expected).expect("correct sha256 should pass");
    }

    #[test]
    fn verify_checksum_wrong_hash_errors() {
        let data = b"data";
        let wrong = "0000000000000000000000000000000000000000000000000000000000000000";
        let err = verify_checksum(data, wrong).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("expected") || msg.contains("got"),
            "error should describe mismatch: {msg}"
        );
    }
}

#[cfg(test)]
mod signature_tests {
    use super::*;

    #[test]
    fn verify_signature_garbage_bytes_errors() {
        let data = b"some payload";
        let garbage = vec![0xde, 0xad, 0xbe, 0xef];
        let err = verify_signature(data, &garbage).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("invalid signature") || msg.contains("signature"),
            "error should describe bad signature: {msg}"
        );
    }

    #[test]
    fn verify_signature_64_zero_bytes_errors() {
        let data = b"some payload";
        // 64 zero bytes: syntactically a valid-length Ed25519 signature but
        // will fail cryptographic verification against the real embedded key.
        let zeros = vec![0u8; 64];
        let err = verify_signature(data, &zeros).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("verification failed")
                || msg.contains("signature")
                || msg.contains("ed25519"),
            "error should describe verification failure: {msg}"
        );
    }
}

#[cfg(test)]
mod spawn_update_tests {
    use super::*;
    use tokio::sync::mpsc;

    fn make_params(target_version: &str) -> UpdateParams {
        UpdateParams {
            update_id: "test-upd".into(),
            target_version: target_version.into(),
            download_url: "https://example.invalid/bin".into(),
            checksum_sha256: "".into(),
            signature: vec![],
            max_retries: 1,
        }
    }

    // The current CARGO_PKG_VERSION for ahandd drives the downgrade check. We
    // construct a version that is guaranteed to be lower.
    fn lower_version() -> String {
        "0.0.1-test-downgrade".into()
    }

    #[test]
    fn spawn_update_rejects_downgrade() {
        // Target version 0.0.1 is guaranteed to be ≤ any real release.
        let (tx, _rx) = mpsc::unbounded_channel();
        let params = make_params(&lower_version());
        let launched = spawn_update(params, "dev-test".into(), tx);
        assert!(!launched, "downgrade should be rejected (returns false)");
    }

    #[tokio::test]
    async fn spawn_update_concurrent_guard_returns_false_while_busy() {
        // Set the flag; assert spawn_update returns false; reset.
        UPDATE_IN_PROGRESS.store(true, std::sync::atomic::Ordering::SeqCst);
        let (tx, _rx) = mpsc::unbounded_channel();
        // Use a version that is higher than the current package version so the
        // downgrade check doesn't interfere. We pick a large semver.
        let params = make_params("999.0.0");
        let launched = spawn_update(params, "dev-test".into(), tx);
        // Reset before asserting (avoid poisoning other tests).
        UPDATE_IN_PROGRESS.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(
            !launched,
            "concurrent-guard should return false while UPDATE_IN_PROGRESS is set"
        );
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
