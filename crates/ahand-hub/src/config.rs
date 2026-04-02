use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum StoreConfig {
    Memory,
    Persistent {
        database_url: String,
        redis_url: String,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub bind_addr: String,
    pub service_token: String,
    pub dashboard_shared_password: String,
    pub dashboard_allowed_origins: Vec<String>,
    pub device_bootstrap_token: String,
    pub device_bootstrap_device_id: String,
    pub device_hello_max_age_ms: u64,
    pub device_heartbeat_interval_ms: u64,
    pub device_heartbeat_timeout_ms: u64,
    pub device_presence_ttl_secs: u64,
    pub device_presence_refresh_ms: u64,
    pub job_timeout_grace_ms: u64,
    pub device_disconnect_grace_ms: u64,
    pub jwt_secret: String,
    pub output_retention_ms: u64,
    pub store: StoreConfig,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_env_with(|key| std::env::var(key).ok())
    }

    fn from_env_with<F>(getenv: F) -> anyhow::Result<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        Ok(Self {
            bind_addr: getenv("AHAND_HUB_BIND_ADDR").unwrap_or_else(|| "127.0.0.1:8080".into()),
            service_token: required_env(&getenv, "AHAND_HUB_SERVICE_TOKEN")?,
            dashboard_shared_password: required_env(&getenv, "AHAND_HUB_DASHBOARD_PASSWORD")?,
            dashboard_allowed_origins: getenv("AHAND_HUB_DASHBOARD_ALLOWED_ORIGINS")
                .map(|value| {
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(std::string::ToString::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            device_bootstrap_token: required_env(&getenv, "AHAND_HUB_DEVICE_BOOTSTRAP_TOKEN")?,
            device_bootstrap_device_id: required_env(
                &getenv,
                "AHAND_HUB_DEVICE_BOOTSTRAP_DEVICE_ID",
            )?,
            device_hello_max_age_ms: getenv("AHAND_HUB_DEVICE_HELLO_MAX_AGE_MS")
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(300_000),
            device_heartbeat_interval_ms: getenv("AHAND_HUB_DEVICE_HEARTBEAT_INTERVAL_MS")
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(30_000),
            device_heartbeat_timeout_ms: getenv("AHAND_HUB_DEVICE_HEARTBEAT_TIMEOUT_MS")
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(90_000),
            device_presence_ttl_secs: getenv("AHAND_HUB_DEVICE_PRESENCE_TTL_SECS")
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(60),
            device_presence_refresh_ms: getenv("AHAND_HUB_DEVICE_PRESENCE_REFRESH_MS")
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(20_000),
            job_timeout_grace_ms: getenv("AHAND_HUB_JOB_TIMEOUT_GRACE_MS")
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(1_000),
            device_disconnect_grace_ms: getenv("AHAND_HUB_DEVICE_DISCONNECT_GRACE_MS")
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(10 * 60 * 1_000),
            jwt_secret: required_env(&getenv, "AHAND_HUB_JWT_SECRET")?,
            output_retention_ms: getenv("AHAND_HUB_OUTPUT_RETENTION_MS")
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(60 * 60 * 1000),
            store: StoreConfig::Persistent {
                database_url: required_env(&getenv, "AHAND_HUB_DATABASE_URL")?,
                redis_url: required_env(&getenv, "AHAND_HUB_REDIS_URL")?,
            },
        })
    }
}

fn required_env<F>(getenv: &F, key: &str) -> anyhow::Result<String>
where
    F: Fn(&str) -> Option<String>,
{
    let value = getenv(key).ok_or_else(|| anyhow::anyhow!("{key} must be set"))?;
    if value.trim().is_empty() {
        return Err(anyhow::anyhow!("{key} must not be blank"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::Config;
    use super::StoreConfig;

    #[test]
    fn from_env_with_requires_secret_inputs() {
        let env = HashMap::<String, String>::new();
        let err = Config::from_env_with(|key| env.get(key).cloned()).unwrap_err();
        assert!(err.to_string().contains("AHAND_HUB_SERVICE_TOKEN"));
    }

    #[test]
    fn from_env_with_reads_required_values_and_defaults_bind_addr() {
        let env = HashMap::from([
            (
                "AHAND_HUB_SERVICE_TOKEN".to_string(),
                "service-prod-token".to_string(),
            ),
            (
                "AHAND_HUB_DASHBOARD_PASSWORD".to_string(),
                "shared-dashboard-password".to_string(),
            ),
            (
                "AHAND_HUB_DEVICE_BOOTSTRAP_TOKEN".to_string(),
                "bootstrap-prod-token".to_string(),
            ),
            (
                "AHAND_HUB_DEVICE_BOOTSTRAP_DEVICE_ID".to_string(),
                "device-prod-1".to_string(),
            ),
            (
                "AHAND_HUB_JWT_SECRET".to_string(),
                "jwt-prod-secret".to_string(),
            ),
            (
                "AHAND_HUB_DATABASE_URL".to_string(),
                "postgres://prod".to_string(),
            ),
            (
                "AHAND_HUB_REDIS_URL".to_string(),
                "redis://prod".to_string(),
            ),
        ]);

        let config = Config::from_env_with(|key| env.get(key).cloned()).unwrap();
        assert_eq!(config.bind_addr, "127.0.0.1:8080");
        assert_eq!(config.service_token, "service-prod-token");
        assert_eq!(
            config.dashboard_shared_password,
            "shared-dashboard-password"
        );
        assert!(config.dashboard_allowed_origins.is_empty());
        assert_eq!(config.device_bootstrap_token, "bootstrap-prod-token");
        assert_eq!(config.device_bootstrap_device_id, "device-prod-1");
        assert_eq!(config.device_hello_max_age_ms, 300_000);
        assert_eq!(config.device_heartbeat_interval_ms, 30_000);
        assert_eq!(config.device_heartbeat_timeout_ms, 90_000);
        assert_eq!(config.device_presence_ttl_secs, 60);
        assert_eq!(config.device_presence_refresh_ms, 20_000);
        assert_eq!(config.job_timeout_grace_ms, 1_000);
        assert_eq!(config.device_disconnect_grace_ms, 10 * 60 * 1_000);
        assert_eq!(config.jwt_secret, "jwt-prod-secret");
        match config.store {
            StoreConfig::Persistent {
                database_url,
                redis_url,
            } => {
                assert_eq!(database_url, "postgres://prod");
                assert_eq!(redis_url, "redis://prod");
            }
            StoreConfig::Memory => panic!("expected persistent store config"),
        }
    }

    #[test]
    fn from_env_with_requires_store_inputs() {
        let env = HashMap::from([
            (
                "AHAND_HUB_SERVICE_TOKEN".to_string(),
                "service-prod-token".to_string(),
            ),
            (
                "AHAND_HUB_DASHBOARD_PASSWORD".to_string(),
                "shared-dashboard-password".to_string(),
            ),
            (
                "AHAND_HUB_DASHBOARD_ALLOWED_ORIGINS".to_string(),
                "https://dashboard.example, https://ops.example".to_string(),
            ),
            (
                "AHAND_HUB_DEVICE_BOOTSTRAP_TOKEN".to_string(),
                "bootstrap-prod-token".to_string(),
            ),
            (
                "AHAND_HUB_DEVICE_BOOTSTRAP_DEVICE_ID".to_string(),
                "device-prod-1".to_string(),
            ),
            (
                "AHAND_HUB_JWT_SECRET".to_string(),
                "jwt-prod-secret".to_string(),
            ),
        ]);

        let err = Config::from_env_with(|key| env.get(key).cloned()).unwrap_err();
        assert!(err.to_string().contains("AHAND_HUB_DATABASE_URL"));
    }

    #[test]
    fn from_env_with_rejects_blank_secret_inputs() {
        let env = HashMap::from([
            ("AHAND_HUB_SERVICE_TOKEN".to_string(), String::new()),
            (
                "AHAND_HUB_DASHBOARD_PASSWORD".to_string(),
                "shared-dashboard-password".to_string(),
            ),
            (
                "AHAND_HUB_DEVICE_BOOTSTRAP_TOKEN".to_string(),
                "bootstrap-prod-token".to_string(),
            ),
            (
                "AHAND_HUB_DEVICE_BOOTSTRAP_DEVICE_ID".to_string(),
                "device-prod-1".to_string(),
            ),
            (
                "AHAND_HUB_JWT_SECRET".to_string(),
                "jwt-prod-secret".to_string(),
            ),
            (
                "AHAND_HUB_DATABASE_URL".to_string(),
                "postgres://prod".to_string(),
            ),
            (
                "AHAND_HUB_REDIS_URL".to_string(),
                "redis://prod".to_string(),
            ),
        ]);

        let err = Config::from_env_with(|key| env.get(key).cloned()).unwrap_err();
        assert!(err.to_string().contains("AHAND_HUB_SERVICE_TOKEN"));
    }

    #[test]
    fn from_env_with_parses_allowed_dashboard_origins() {
        let env = HashMap::from([
            (
                "AHAND_HUB_SERVICE_TOKEN".to_string(),
                "service-prod-token".to_string(),
            ),
            (
                "AHAND_HUB_DASHBOARD_PASSWORD".to_string(),
                "shared-dashboard-password".to_string(),
            ),
            (
                "AHAND_HUB_DASHBOARD_ALLOWED_ORIGINS".to_string(),
                "https://dashboard.example, https://ops.example".to_string(),
            ),
            (
                "AHAND_HUB_DEVICE_BOOTSTRAP_TOKEN".to_string(),
                "bootstrap-prod-token".to_string(),
            ),
            (
                "AHAND_HUB_DEVICE_BOOTSTRAP_DEVICE_ID".to_string(),
                "device-prod-1".to_string(),
            ),
            (
                "AHAND_HUB_JWT_SECRET".to_string(),
                "jwt-prod-secret".to_string(),
            ),
            (
                "AHAND_HUB_DATABASE_URL".to_string(),
                "postgres://prod".to_string(),
            ),
            (
                "AHAND_HUB_REDIS_URL".to_string(),
                "redis://prod".to_string(),
            ),
        ]);

        let config = Config::from_env_with(|key| env.get(key).cloned()).unwrap();
        assert_eq!(
            config.dashboard_allowed_origins,
            vec![
                "https://dashboard.example".to_string(),
                "https://ops.example".to_string()
            ]
        );
    }
}
