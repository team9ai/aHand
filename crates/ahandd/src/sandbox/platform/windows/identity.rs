//! Windows sandbox identity loading helpers.

use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

use super::setup::{
    OfflineProxySettings, SandboxNetworkIdentity, SandboxUserRecord, SandboxUsersFile, SetupMarker,
    offline_proxy_settings_from_env, sandbox_users_path, setup_marker_path,
};
use super::setup_error::{SetupErrorCode, SetupFailure};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SandboxCreds {
    pub(super) username: String,
    pub(super) password: String,
}

#[allow(dead_code)]
pub(super) fn sandbox_setup_is_complete(state_root: &Path) -> bool {
    let marker_ok = matches!(load_marker(state_root), Ok(Some(marker)) if marker.version_matches() && marker.usernames_match() && marker.hard_network_block_ready());
    if !marker_ok {
        return false;
    }
    matches!(load_users(state_root), Ok(Some(users)) if users.version_matches())
}

pub(super) fn load_sandbox_creds_for_identity(
    network_identity: SandboxNetworkIdentity,
    state_root: &Path,
    env: &HashMap<String, String>,
) -> Result<SandboxCreds, SetupFailure> {
    let marker = load_marker(state_root)?.ok_or_else(|| {
        SetupFailure::unavailable(format!(
            "sandbox setup marker missing at {}",
            setup_marker_path(state_root).display()
        ))
    })?;
    if !marker.version_matches() {
        return Err(SetupFailure::unavailable(format!(
            "sandbox setup marker version {} does not match required version {}",
            marker.version,
            super::setup::SETUP_VERSION
        )));
    }
    if !marker.usernames_match() {
        return Err(SetupFailure::unavailable(format!(
            "sandbox setup marker uses unexpected usernames offline={} online={}",
            marker.offline_username, marker.online_username
        )));
    }
    if network_identity.uses_offline_identity() && !marker.hard_network_block_ready() {
        return Err(SetupFailure::unavailable(
            "offline sandbox hard network block is not marked verified",
        ));
    }

    let desired_proxy_settings = offline_proxy_settings_from_env(env, network_identity);
    if let Some(reason) = marker.request_mismatch_reason(network_identity, &desired_proxy_settings)
    {
        return Err(SetupFailure::unavailable(reason));
    }

    let users = load_users(state_root)?.ok_or_else(|| {
        SetupFailure::unavailable(format!(
            "sandbox users file missing at {}",
            sandbox_users_path(state_root).display()
        ))
    })?;
    if !users.version_matches() {
        return Err(SetupFailure::unavailable(format!(
            "sandbox users file version {} does not match required version {}",
            users.version,
            super::setup::SETUP_VERSION
        )));
    }

    let selected = match network_identity {
        SandboxNetworkIdentity::Offline => users.offline,
        SandboxNetworkIdentity::Online => users.online,
    };
    let password = decode_password(&selected)?;
    Ok(SandboxCreds {
        username: selected.username,
        password,
    })
}

fn load_marker(state_root: &Path) -> Result<Option<SetupMarker>, SetupFailure> {
    let path = setup_marker_path(state_root);
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(SetupFailure::new(
                SetupErrorCode::MarkerReadFailed,
                format!("failed to read {}: {err}", path.display()),
            ));
        }
    };
    serde_json::from_str(&contents).map(Some).map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::MarkerDecodeFailed,
            format!("failed to decode {}: {err}", path.display()),
        )
    })
}

fn load_users(state_root: &Path) -> Result<Option<SandboxUsersFile>, SetupFailure> {
    let path = sandbox_users_path(state_root);
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(SetupFailure::new(
                SetupErrorCode::UsersReadFailed,
                format!("failed to read {}: {err}", path.display()),
            ));
        }
    };
    serde_json::from_str(&contents).map(Some).map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::UsersDecodeFailed,
            format!("failed to decode {}: {err}", path.display()),
        )
    })
}

fn decode_password(record: &SandboxUserRecord) -> Result<String, SetupFailure> {
    #[cfg(test)]
    if let Some(plaintext) = record.password.strip_prefix("test-plain:") {
        return Ok(plaintext.to_string());
    }

    let blob = BASE64_STANDARD
        .decode(record.password.as_bytes())
        .map_err(|err| {
            SetupFailure::new(
                SetupErrorCode::PasswordDecodeFailed,
                format!(
                    "failed to base64-decode sandbox password for {}: {err}",
                    record.username
                ),
            )
        })?;
    let decrypted = super::dpapi::unprotect(&blob)?;
    String::from_utf8(decrypted).map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::PasswordDecodeFailed,
            format!(
                "sandbox password for {} is not utf-8: {err}",
                record.username
            ),
        )
    })
}

#[allow(dead_code)]
fn _offline_proxy_settings_type_check(_: &OfflineProxySettings) {}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;

    use serde_json::json;

    use super::super::setup::{
        OFFLINE_USERNAME, ONLINE_USERNAME, SETUP_VERSION, SandboxNetworkIdentity, sandbox_dir,
        sandbox_secrets_dir,
    };
    use super::*;

    fn write_marker(
        state_root: &Path,
        version: u32,
        proxy_ports: Vec<u16>,
        allow_local_binding: bool,
        hard_network_block: bool,
    ) {
        fs::create_dir_all(sandbox_dir(state_root)).unwrap();
        fs::write(
            sandbox_dir(state_root).join("setup_marker.json"),
            serde_json::to_vec_pretty(&json!({
                "version": version,
                "offline_username": OFFLINE_USERNAME,
                "online_username": ONLINE_USERNAME,
                "created_at": "2026-06-24T00:00:00Z",
                "hard_network_block": hard_network_block,
                "proxy_ports": proxy_ports,
                "allow_local_binding": allow_local_binding,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn write_users(state_root: &Path, version: u32) {
        fs::create_dir_all(sandbox_secrets_dir(state_root)).unwrap();
        fs::write(
            sandbox_secrets_dir(state_root).join("sandbox_users.json"),
            serde_json::to_vec_pretty(&json!({
                "version": version,
                "offline": {
                    "username": OFFLINE_USERNAME,
                    "password": "test-plain:offline-password",
                },
                "online": {
                    "username": ONLINE_USERNAME,
                    "password": "test-plain:online-password",
                },
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn setup_incomplete_when_marker_or_users_are_missing() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!sandbox_setup_is_complete(temp.path()));

        write_marker(temp.path(), SETUP_VERSION, vec![], false, false);
        assert!(!sandbox_setup_is_complete(temp.path()));
    }

    #[test]
    fn setup_complete_when_valid_marker_users_and_hard_block_readiness_exist() {
        let temp = tempfile::tempdir().unwrap();
        write_marker(temp.path(), SETUP_VERSION, vec![], false, true);
        write_users(temp.path(), SETUP_VERSION);

        assert!(sandbox_setup_is_complete(temp.path()));
    }

    #[test]
    fn offline_identity_rejects_marker_proxy_drift() {
        let temp = tempfile::tempdir().unwrap();
        write_marker(temp.path(), SETUP_VERSION, vec![8080], false, true);
        write_users(temp.path(), SETUP_VERSION);

        let env = HashMap::new();
        let err =
            load_sandbox_creds_for_identity(SandboxNetworkIdentity::Offline, temp.path(), &env)
                .unwrap_err();

        assert_eq!(
            err.code,
            super::super::setup_error::SetupErrorCode::SetupUnavailable
        );
        assert!(err.message.contains("offline firewall settings changed"));
    }

    #[test]
    fn online_identity_loads_online_creds_without_offline_proxy_match() {
        let temp = tempfile::tempdir().unwrap();
        write_marker(temp.path(), SETUP_VERSION, vec![8080], true, false);
        write_users(temp.path(), SETUP_VERSION);

        let env = HashMap::from([(
            "HTTP_PROXY".to_string(),
            "http://127.0.0.1:3128".to_string(),
        )]);
        let creds =
            load_sandbox_creds_for_identity(SandboxNetworkIdentity::Online, temp.path(), &env)
                .unwrap();

        assert_eq!(creds.username, ONLINE_USERNAME);
        assert_eq!(creds.password, "online-password");
    }

    #[test]
    fn offline_identity_rejects_marker_without_hard_network_block_readiness() {
        let temp = tempfile::tempdir().unwrap();
        write_marker(temp.path(), SETUP_VERSION, vec![], false, false);
        write_users(temp.path(), SETUP_VERSION);

        let err = load_sandbox_creds_for_identity(
            SandboxNetworkIdentity::Offline,
            temp.path(),
            &HashMap::new(),
        )
        .unwrap_err();

        assert_eq!(
            err.code,
            super::super::setup_error::SetupErrorCode::SetupUnavailable
        );
        assert!(err.message.contains("hard network block"));
    }

    #[test]
    fn offline_identity_loads_creds_when_hard_network_block_ready() {
        let temp = tempfile::tempdir().unwrap();
        write_marker(temp.path(), SETUP_VERSION, vec![], false, false);
        write_users(temp.path(), SETUP_VERSION);

        let marker_path = sandbox_dir(temp.path()).join("setup_marker.json");
        let mut marker: serde_json::Value =
            serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
        marker["hard_network_block"] = serde_json::Value::Bool(true);
        fs::write(&marker_path, serde_json::to_vec_pretty(&marker).unwrap()).unwrap();

        let creds = load_sandbox_creds_for_identity(
            SandboxNetworkIdentity::Offline,
            temp.path(),
            &HashMap::new(),
        )
        .unwrap();

        assert_eq!(creds.username, OFFLINE_USERNAME);
        assert_eq!(creds.password, "offline-password");
    }
}
