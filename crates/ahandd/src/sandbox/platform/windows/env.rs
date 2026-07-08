//! Windows process environment normalization for sandboxed commands.

use std::collections::HashMap;

use crate::sandbox::types::{NetworkPolicy, SandboxError, SandboxResult};

pub(super) fn normalize_env(
    mut env: HashMap<String, String>,
    network: NetworkPolicy,
) -> SandboxResult<HashMap<String, String>> {
    insert_default(&mut env, "PAGER", "more.com");
    insert_default(&mut env, "GIT_PAGER", "more.com");
    insert_default(&mut env, "LESS", "");
    insert_default(&mut env, "NULL_DEVICE", "NUL");
    inherit_case_insensitive(&mut env, "PATH", "");
    inherit_case_insensitive(&mut env, "PATHEXT", ".COM;.EXE;.BAT;.CMD");

    match network {
        NetworkPolicy::Enabled => {}
        NetworkPolicy::Disabled => apply_no_network_to_env(&mut env),
        NetworkPolicy::ProxyOnly => {
            return Err(SandboxError::unavailable(
                "NetworkPolicy::ProxyOnly is not supported by the aHand sandbox yet",
            ));
        }
    }

    Ok(env)
}

fn insert_default(env: &mut HashMap<String, String>, key: &str, value: &str) {
    if !contains_key_ignore_ascii_case(env, key) {
        env.insert(key.to_string(), value.to_string());
    }
}

fn contains_key_ignore_ascii_case(env: &HashMap<String, String>, key: &str) -> bool {
    env.keys()
        .any(|existing| existing.eq_ignore_ascii_case(key))
}

fn inherit_case_insensitive(env: &mut HashMap<String, String>, key: &str, fallback: &str) {
    if contains_key_ignore_ascii_case(env, key) {
        return;
    }

    let value = std::env::var(key).unwrap_or_else(|_| fallback.to_string());
    env.insert(key.to_string(), value);
}

fn apply_no_network_to_env(env: &mut HashMap<String, String>) {
    set_canonical_case_insensitive(env, "SBX_NONET_ACTIVE", "1");
    set_canonical_case_insensitive(env, "HTTP_PROXY", "http://127.0.0.1:9");
    set_canonical_case_insensitive(env, "HTTPS_PROXY", "http://127.0.0.1:9");
    set_canonical_case_insensitive(env, "ALL_PROXY", "http://127.0.0.1:9");
    set_canonical_case_insensitive(env, "NO_PROXY", "localhost,127.0.0.1,::1");
    set_canonical_case_insensitive(env, "PIP_NO_INDEX", "1");
    set_canonical_case_insensitive(env, "PIP_DISABLE_PIP_VERSION_CHECK", "1");
    set_canonical_case_insensitive(env, "NPM_CONFIG_OFFLINE", "true");
    set_canonical_case_insensitive(env, "CARGO_NET_OFFLINE", "true");
    set_canonical_case_insensitive(env, "GIT_HTTP_PROXY", "http://127.0.0.1:9");
    set_canonical_case_insensitive(env, "GIT_HTTPS_PROXY", "http://127.0.0.1:9");
    set_canonical_case_insensitive(env, "GIT_SSH_COMMAND", "cmd /c exit 1");
    set_canonical_case_insensitive(env, "GIT_ALLOW_PROTOCOLS", "");
}

fn set_canonical_case_insensitive(env: &mut HashMap<String, String>, key: &str, value: &str) {
    env.retain(|existing, _| !existing.eq_ignore_ascii_case(key));
    env.insert(key.to_string(), value.to_string());
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::sandbox::types::NetworkPolicy;

    use super::*;

    #[test]
    fn inserts_windows_defaults() {
        let env = normalize_env(HashMap::new(), NetworkPolicy::Enabled).unwrap();

        assert_eq!(env.get("PAGER").map(String::as_str), Some("more.com"));
        assert_eq!(env.get("GIT_PAGER").map(String::as_str), Some("more.com"));
        assert_eq!(env.get("LESS").map(String::as_str), Some(""));
        assert_eq!(env.get("NULL_DEVICE").map(String::as_str), Some("NUL"));
        assert!(env.keys().any(|key| key.eq_ignore_ascii_case("PATH")));
        assert!(env.keys().any(|key| key.eq_ignore_ascii_case("PATHEXT")));
    }

    #[test]
    fn disabled_network_marks_environment_for_no_network() {
        let env = normalize_env(HashMap::new(), NetworkPolicy::Disabled).unwrap();

        assert_eq!(env.get("SBX_NONET_ACTIVE").map(String::as_str), Some("1"));
        assert_eq!(
            env.get("HTTP_PROXY").map(String::as_str),
            Some("http://127.0.0.1:9")
        );
        assert_eq!(
            env.get("NPM_CONFIG_OFFLINE").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn disabled_network_overrides_lowercase_proxy_values() {
        let env = normalize_env(
            HashMap::from([(
                "http_proxy".to_string(),
                "http://127.0.0.1:8080".to_string(),
            )]),
            NetworkPolicy::Disabled,
        )
        .unwrap();

        assert_eq!(
            env.get("HTTP_PROXY").map(String::as_str),
            Some("http://127.0.0.1:9")
        );
        assert!(!env.contains_key("http_proxy"));
    }

    #[test]
    fn disabled_network_overrides_lowercase_nonet_marker() {
        let env = normalize_env(
            HashMap::from([("sbx_nonet_active".to_string(), "0".to_string())]),
            NetworkPolicy::Disabled,
        )
        .unwrap();

        assert_eq!(env.get("SBX_NONET_ACTIVE").map(String::as_str), Some("1"));
        assert!(!env.contains_key("sbx_nonet_active"));
    }

    #[test]
    fn proxy_only_network_policy_is_rejected_by_env_normalization() {
        let err = normalize_env(HashMap::new(), NetworkPolicy::ProxyOnly).unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
        assert!(err.message.contains("ProxyOnly"));
    }
}
