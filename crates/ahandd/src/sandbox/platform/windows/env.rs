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
    env.insert("SBX_NONET_ACTIVE".to_string(), "1".to_string());
    insert_default(env, "HTTP_PROXY", "http://127.0.0.1:9");
    insert_default(env, "HTTPS_PROXY", "http://127.0.0.1:9");
    insert_default(env, "ALL_PROXY", "http://127.0.0.1:9");
    insert_default(env, "NO_PROXY", "localhost,127.0.0.1,::1");
    insert_default(env, "PIP_NO_INDEX", "1");
    insert_default(env, "PIP_DISABLE_PIP_VERSION_CHECK", "1");
    insert_default(env, "NPM_CONFIG_OFFLINE", "true");
    insert_default(env, "CARGO_NET_OFFLINE", "true");
    insert_default(env, "GIT_HTTP_PROXY", "http://127.0.0.1:9");
    insert_default(env, "GIT_HTTPS_PROXY", "http://127.0.0.1:9");
    insert_default(env, "GIT_SSH_COMMAND", "cmd /c exit 1");
    insert_default(env, "GIT_ALLOW_PROTOCOLS", "");
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
    fn proxy_only_network_policy_is_rejected_by_env_normalization() {
        let err = normalize_env(HashMap::new(), NetworkPolicy::ProxyOnly).unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
        assert!(err.message.contains("ProxyOnly"));
    }
}
