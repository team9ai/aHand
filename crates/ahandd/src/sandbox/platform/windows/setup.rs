//! Windows sandbox setup orchestration helpers.

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::network::WindowsNetworkMode;
use crate::sandbox::types::{SandboxError, SandboxResult};

pub(super) const SETUP_VERSION: u32 = 1;
pub(super) const OFFLINE_USERNAME: &str = "AhandSandboxOffline";
pub(super) const ONLINE_USERNAME: &str = "AhandSandboxOnline";

pub(super) fn sandbox_dir(state_root: &Path) -> PathBuf {
    state_root.join(".sandbox")
}

pub(super) fn sandbox_secrets_dir(state_root: &Path) -> PathBuf {
    state_root.join(".sandbox-secrets")
}

pub(super) fn setup_marker_path(state_root: &Path) -> PathBuf {
    sandbox_dir(state_root).join("setup_marker.json")
}

pub(super) fn sandbox_users_path(state_root: &Path) -> PathBuf {
    sandbox_secrets_dir(state_root).join("sandbox_users.json")
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(super) struct SetupMarker {
    pub(super) version: u32,
    pub(super) offline_username: String,
    pub(super) online_username: String,
    #[serde(default)]
    pub(super) created_at: Option<String>,
    #[serde(default)]
    pub(super) proxy_ports: Vec<u16>,
    #[serde(default)]
    pub(super) allow_local_binding: bool,
}

impl SetupMarker {
    pub(super) fn version_matches(&self) -> bool {
        self.version == SETUP_VERSION
    }

    pub(super) fn usernames_match(&self) -> bool {
        self.offline_username == OFFLINE_USERNAME && self.online_username == ONLINE_USERNAME
    }

    pub(super) fn request_mismatch_reason(
        &self,
        network_identity: SandboxNetworkIdentity,
        offline_proxy_settings: &OfflineProxySettings,
    ) -> Option<String> {
        if !network_identity.uses_offline_identity() {
            return None;
        }
        if self.proxy_ports == offline_proxy_settings.proxy_ports
            && self.allow_local_binding == offline_proxy_settings.allow_local_binding
        {
            return None;
        }
        Some(format!(
            "offline firewall settings changed (stored_ports={:?}, desired_ports={:?}, stored_allow_local_binding={}, desired_allow_local_binding={})",
            self.proxy_ports,
            offline_proxy_settings.proxy_ports,
            self.allow_local_binding,
            offline_proxy_settings.allow_local_binding
        ))
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(super) struct SandboxUserRecord {
    pub(super) username: String,
    pub(super) password: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(super) struct SandboxUsersFile {
    pub(super) version: u32,
    pub(super) offline: SandboxUserRecord,
    pub(super) online: SandboxUserRecord,
}

impl SandboxUsersFile {
    pub(super) fn version_matches(&self) -> bool {
        self.version == SETUP_VERSION
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(super) enum SandboxNetworkIdentity {
    Offline,
    Online,
}

impl SandboxNetworkIdentity {
    pub(super) fn uses_offline_identity(self) -> bool {
        matches!(self, Self::Offline)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OfflineProxySettings {
    pub(super) proxy_ports: Vec<u16>,
    pub(super) allow_local_binding: bool,
}

const PROXY_ENV_KEYS: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "WS_PROXY",
    "WSS_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "ws_proxy",
    "wss_proxy",
];
const ALLOW_LOCAL_BINDING_ENV_KEY: &str = "AHAND_NETWORK_ALLOW_LOCAL_BINDING";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WindowsNetworkContext {
    pub(super) mode: WindowsNetworkMode,
    pub(super) state_root: PathBuf,
    pub(super) sandbox_creds: Option<super::identity::SandboxCreds>,
}

pub(super) fn prepare_network_context(
    mode: WindowsNetworkMode,
    env: &HashMap<String, String>,
    sandbox_state_root: &Path,
) -> SandboxResult<WindowsNetworkContext> {
    match mode {
        WindowsNetworkMode::Online => Ok(WindowsNetworkContext {
            mode,
            state_root: sandbox_state_root.to_path_buf(),
            sandbox_creds: None,
        }),
        WindowsNetworkMode::Offline => {
            match super::identity::load_sandbox_creds_for_identity(
                SandboxNetworkIdentity::Offline,
                sandbox_state_root,
                env,
            ) {
                Ok(_) => Err(SandboxError::unavailable(
                    "NetworkPolicy::Disabled hard network blocking is not implemented on Windows",
                )),
                Err(err) => Err(SandboxError::unavailable(format!(
                    "NetworkPolicy::Disabled hard network blocking/setup is unavailable or incomplete on Windows: {err}"
                ))),
            }
        }
    }
}

pub(super) fn offline_proxy_settings_from_env(
    env_map: &HashMap<String, String>,
    network_identity: SandboxNetworkIdentity,
) -> OfflineProxySettings {
    if !network_identity.uses_offline_identity() {
        return OfflineProxySettings {
            proxy_ports: vec![],
            allow_local_binding: false,
        };
    }
    OfflineProxySettings {
        proxy_ports: proxy_ports_from_env(env_map),
        allow_local_binding: env_map
            .get(ALLOW_LOCAL_BINDING_ENV_KEY)
            .is_some_and(|value| value == "1"),
    }
}

pub(super) fn proxy_ports_from_env(env_map: &HashMap<String, String>) -> Vec<u16> {
    let mut ports = BTreeSet::new();
    for key in PROXY_ENV_KEYS {
        if let Some(value) = env_map.get(*key)
            && let Some(port) = loopback_proxy_port_from_url(value)
        {
            ports.insert(port);
        }
    }
    ports.into_iter().collect()
}

fn loopback_proxy_port_from_url(url: &str) -> Option<u16> {
    let authority = url.trim().split_once("://")?.1.split('/').next()?;
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);

    if let Some(host) = host_port.strip_prefix('[') {
        let (host, rest) = host.split_once(']')?;
        if host != "::1" {
            return None;
        }
        let port = rest.strip_prefix(':')?.parse::<u16>().ok()?;
        return (port != 0).then_some(port);
    }

    let (host, port) = host_port.rsplit_once(':')?;
    if !(host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1") {
        return None;
    }
    let port = port.parse::<u16>().ok()?;
    (port != 0).then_some(port)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;

    use super::super::network::WindowsNetworkMode;
    use super::*;

    fn write_valid_test_setup_state(state_root: &Path) {
        fs::create_dir_all(sandbox_dir(state_root)).unwrap();
        fs::write(
            setup_marker_path(state_root),
            serde_json::json!({
                "version": SETUP_VERSION,
                "offline_username": OFFLINE_USERNAME,
                "online_username": ONLINE_USERNAME,
                "created_at": "2026-06-24T00:00:00Z",
                "proxy_ports": [],
                "allow_local_binding": false,
            })
            .to_string(),
        )
        .unwrap();

        fs::create_dir_all(sandbox_secrets_dir(state_root)).unwrap();
        fs::write(
            sandbox_users_path(state_root),
            serde_json::json!({
                "version": SETUP_VERSION,
                "offline": {
                    "username": OFFLINE_USERNAME,
                    "password": "test-plain:offline-password",
                },
                "online": {
                    "username": ONLINE_USERNAME,
                    "password": "test-plain:online-password",
                },
            })
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn online_network_context_succeeds_without_setup() {
        let temp = tempfile::tempdir().unwrap();
        let context =
            prepare_network_context(WindowsNetworkMode::Online, &HashMap::new(), temp.path())
                .unwrap();

        assert_eq!(context.mode, WindowsNetworkMode::Online);
        assert!(context.sandbox_creds.is_none());
    }

    #[test]
    fn offline_network_context_fails_closed_until_hard_block_exists() {
        let temp = tempfile::tempdir().unwrap();
        let err =
            prepare_network_context(WindowsNetworkMode::Offline, &HashMap::new(), temp.path())
                .unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
        assert!(err.message.contains("Disabled"));
        assert!(err.message.contains("hard network blocking"));
        assert!(err.message.contains("setup is unavailable or incomplete"));
    }

    #[test]
    fn offline_network_context_still_fails_closed_when_identity_state_exists() {
        let temp = tempfile::tempdir().unwrap();
        write_valid_test_setup_state(temp.path());

        let err =
            prepare_network_context(WindowsNetworkMode::Offline, &HashMap::new(), temp.path())
                .unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
        assert!(err.message.contains("hard network blocking"));
        assert!(err.message.contains("not implemented"));
    }

    #[test]
    fn loopback_proxy_url_parsing_supports_common_forms() {
        assert_eq!(
            loopback_proxy_port_from_url("http://localhost:3128"),
            Some(3128)
        );
        assert_eq!(
            loopback_proxy_port_from_url("https://127.0.0.1:8080"),
            Some(8080)
        );
        assert_eq!(
            loopback_proxy_port_from_url("socks5h://user:pass@[::1]:1080"),
            Some(1080)
        );
    }

    #[test]
    fn loopback_proxy_url_parsing_rejects_non_loopback_and_zero_port() {
        assert_eq!(
            loopback_proxy_port_from_url("http://example.com:3128"),
            None
        );
        assert_eq!(loopback_proxy_port_from_url("http://127.0.0.1:0"), None);
        assert_eq!(loopback_proxy_port_from_url("localhost:8080"), None);
    }

    #[test]
    fn proxy_ports_from_env_dedupes_and_sorts() {
        let env = HashMap::from([
            (
                "HTTP_PROXY".to_string(),
                "http://127.0.0.1:8080".to_string(),
            ),
            (
                "http_proxy".to_string(),
                "http://localhost:8080".to_string(),
            ),
            ("ALL_PROXY".to_string(), "socks5h://[::1]:1081".to_string()),
            (
                "HTTPS_PROXY".to_string(),
                "https://example.com:9999".to_string(),
            ),
        ]);

        assert_eq!(proxy_ports_from_env(&env), vec![1081, 8080]);
    }

    #[test]
    fn offline_proxy_settings_ignore_proxy_env_when_online_identity_selected() {
        let env = HashMap::from([
            (
                "HTTP_PROXY".to_string(),
                "http://127.0.0.1:8080".to_string(),
            ),
            (
                "AHAND_NETWORK_ALLOW_LOCAL_BINDING".to_string(),
                "1".to_string(),
            ),
        ]);

        assert_eq!(
            offline_proxy_settings_from_env(&env, SandboxNetworkIdentity::Online),
            OfflineProxySettings {
                proxy_ports: vec![],
                allow_local_binding: false,
            }
        );
    }

    #[test]
    fn offline_proxy_settings_capture_proxy_ports_and_local_binding_for_offline_identity() {
        let env = HashMap::from([
            (
                "HTTP_PROXY".to_string(),
                "http://127.0.0.1:8080".to_string(),
            ),
            (
                "ALL_PROXY".to_string(),
                "socks5h://127.0.0.1:1081".to_string(),
            ),
            (
                "AHAND_NETWORK_ALLOW_LOCAL_BINDING".to_string(),
                "1".to_string(),
            ),
        ]);

        assert_eq!(
            offline_proxy_settings_from_env(&env, SandboxNetworkIdentity::Offline),
            OfflineProxySettings {
                proxy_ports: vec![1081, 8080],
                allow_local_binding: true,
            }
        );
    }

    #[test]
    fn setup_marker_request_mismatch_reason_ignores_proxy_drift_for_online_identity() {
        let marker = SetupMarker {
            version: SETUP_VERSION,
            offline_username: OFFLINE_USERNAME.to_string(),
            online_username: ONLINE_USERNAME.to_string(),
            created_at: None,
            proxy_ports: vec![3128],
            allow_local_binding: false,
        };
        let desired = OfflineProxySettings {
            proxy_ports: vec![1081, 8080],
            allow_local_binding: true,
        };

        assert_eq!(
            marker.request_mismatch_reason(SandboxNetworkIdentity::Online, &desired),
            None
        );
    }

    #[test]
    fn setup_marker_request_mismatch_reason_reports_offline_firewall_drift() {
        let marker = SetupMarker {
            version: SETUP_VERSION,
            offline_username: OFFLINE_USERNAME.to_string(),
            online_username: ONLINE_USERNAME.to_string(),
            created_at: None,
            proxy_ports: vec![3128],
            allow_local_binding: false,
        };
        let desired = OfflineProxySettings {
            proxy_ports: vec![1081, 8080],
            allow_local_binding: true,
        };

        assert_eq!(
            marker.request_mismatch_reason(SandboxNetworkIdentity::Offline, &desired),
            Some(
                "offline firewall settings changed (stored_ports=[3128], desired_ports=[1081, 8080], stored_allow_local_binding=false, desired_allow_local_binding=true)"
                    .to_string()
            )
        );
    }
}
