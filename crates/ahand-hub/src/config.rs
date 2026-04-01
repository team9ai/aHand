use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub bind_addr: String,
    pub service_token: String,
    pub device_bootstrap_token: String,
    pub device_bootstrap_device_id: String,
    pub device_hello_max_age_ms: u64,
    pub jwt_secret: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_env_with(|key| std::env::var(key).ok())
    }

    pub fn for_tests() -> Self {
        Self {
            bind_addr: "127.0.0.1:0".into(),
            service_token: "service-test-token".into(),
            device_bootstrap_token: "bootstrap-test-token".into(),
            device_bootstrap_device_id: "device-2".into(),
            device_hello_max_age_ms: 30_000,
            jwt_secret: "service-test-secret".into(),
        }
    }

    fn from_env_with<F>(getenv: F) -> anyhow::Result<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        Ok(Self {
            bind_addr: getenv("AHAND_HUB_BIND_ADDR").unwrap_or_else(|| "127.0.0.1:8080".into()),
            service_token: required_env(&getenv, "AHAND_HUB_SERVICE_TOKEN")?,
            device_bootstrap_token: required_env(&getenv, "AHAND_HUB_DEVICE_BOOTSTRAP_TOKEN")?,
            device_bootstrap_device_id: required_env(
                &getenv,
                "AHAND_HUB_DEVICE_BOOTSTRAP_DEVICE_ID",
            )?,
            device_hello_max_age_ms: getenv("AHAND_HUB_DEVICE_HELLO_MAX_AGE_MS")
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(30_000),
            jwt_secret: required_env(&getenv, "AHAND_HUB_JWT_SECRET")?,
        })
    }
}

fn required_env<F>(getenv: &F, key: &str) -> anyhow::Result<String>
where
    F: Fn(&str) -> Option<String>,
{
    getenv(key).ok_or_else(|| anyhow::anyhow!("{key} must be set"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::Config;

    #[test]
    fn from_env_with_requires_secret_inputs() {
        let env = HashMap::<String, String>::new();
        let err = Config::from_env_with(|key| env.get(key).cloned()).unwrap_err();
        assert!(err.to_string().contains("AHAND_HUB_SERVICE_TOKEN"));
    }

    #[test]
    fn from_env_with_reads_required_values_and_defaults_bind_addr() {
        let env = HashMap::from([
            ("AHAND_HUB_SERVICE_TOKEN".to_string(), "service-prod-token".to_string()),
            (
                "AHAND_HUB_DEVICE_BOOTSTRAP_TOKEN".to_string(),
                "bootstrap-prod-token".to_string(),
            ),
            (
                "AHAND_HUB_DEVICE_BOOTSTRAP_DEVICE_ID".to_string(),
                "device-prod-1".to_string(),
            ),
            ("AHAND_HUB_JWT_SECRET".to_string(), "jwt-prod-secret".to_string()),
        ]);

        let config = Config::from_env_with(|key| env.get(key).cloned()).unwrap();
        assert_eq!(config.bind_addr, "127.0.0.1:8080");
        assert_eq!(config.service_token, "service-prod-token");
        assert_eq!(config.device_bootstrap_token, "bootstrap-prod-token");
        assert_eq!(config.device_bootstrap_device_id, "device-prod-1");
        assert_eq!(config.jwt_secret, "jwt-prod-secret");
    }
}
