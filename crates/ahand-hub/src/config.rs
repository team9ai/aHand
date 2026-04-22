use std::path::PathBuf;

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
    /// Cadence used by the server-side staleness monitor to recheck
    /// `last_inbound_at`. No longer a client-ping interval — the daemon
    /// pushes heartbeats — but reused to set the probe loop period.
    pub device_staleness_probe_interval_ms: u64,
    /// Duration without any inbound frame (including heartbeats) after
    /// which the hub closes the WebSocket as dead.
    pub device_staleness_timeout_ms: u64,
    /// Expected daemon heartbeat cadence in seconds. Used to compute the
    /// `presenceTtlSeconds` hint included in `device.heartbeat` webhook
    /// payloads (`secs × 3`). Cadence wording is intentional: there is no
    /// hub-side timer anymore — the daemon owns the schedule.
    pub device_expected_heartbeat_secs: u64,
    pub device_presence_ttl_secs: u64,
    pub device_presence_refresh_ms: u64,
    pub job_timeout_grace_ms: u64,
    pub device_disconnect_grace_ms: u64,
    pub jwt_secret: String,
    pub audit_retention_days: u64,
    pub audit_fallback_path: PathBuf,
    pub output_retention_ms: u64,
    /// Outbound webhook target. When `None`, webhook dispatch is a
    /// no-op: enqueue helpers return immediately and no worker runs.
    /// This matches the plan's "hub runs fine without a configured
    /// gateway" contract for memory-mode and for deployments that
    /// haven't finished integrating team9 yet.
    pub webhook_url: Option<String>,
    /// Shared HMAC-SHA256 secret. Required when `webhook_url` is
    /// Some; `from_env` rejects configurations that set one without
    /// the other.
    pub webhook_secret: Option<String>,
    /// Max retry attempts before a delivery is moved to the DLQ.
    /// Exponential backoff caps at 256s, so the worst-case age of a
    /// row in the queue is `sum(2^i for i in 1..=webhook_max_retries)`
    /// bounded by `webhook_max_retries * 256s`.
    pub webhook_max_retries: u32,
    /// Cap on concurrent in-flight HTTP requests. 1000 qps bursts
    /// from the plan's edge test must not spawn 1000 tasks —
    /// `Semaphore(webhook_max_concurrency)` clamps it.
    pub webhook_max_concurrency: usize,
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
        let config = Self {
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
            device_staleness_probe_interval_ms: getenv(
                "AHAND_HUB_DEVICE_STALENESS_PROBE_INTERVAL_MS",
            )
            .map(|value| value.parse())
            .transpose()?
            .unwrap_or(30_000),
            device_staleness_timeout_ms: getenv("AHAND_HUB_DEVICE_STALENESS_TIMEOUT_MS")
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(180_000),
            device_expected_heartbeat_secs: getenv(
                "AHAND_HUB_DEVICE_EXPECTED_HEARTBEAT_SECS",
            )
            .map(|value| value.parse())
            .transpose()?
            .unwrap_or(60),
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
            audit_retention_days: getenv("AHAND_HUB_AUDIT_RETENTION_DAYS")
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(90),
            audit_fallback_path: getenv("AHAND_HUB_AUDIT_FALLBACK_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(default_audit_fallback_path),
            output_retention_ms: getenv("AHAND_HUB_OUTPUT_RETENTION_MS")
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(60 * 60 * 1000),
            webhook_url: getenv("AHAND_HUB_WEBHOOK_URL").filter(|v| !v.trim().is_empty()),
            webhook_secret: getenv("AHAND_HUB_WEBHOOK_SECRET")
                .filter(|v| !v.trim().is_empty()),
            webhook_max_retries: getenv("AHAND_HUB_WEBHOOK_MAX_RETRIES")
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(8),
            webhook_max_concurrency: getenv("AHAND_HUB_WEBHOOK_MAX_CONCURRENCY")
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(50),
            store: StoreConfig::Persistent {
                database_url: required_env(&getenv, "AHAND_HUB_DATABASE_URL")?,
                redis_url: required_env(&getenv, "AHAND_HUB_REDIS_URL")?,
            },
        };
        if config.webhook_url.is_some() && config.webhook_secret.is_none() {
            return Err(anyhow::anyhow!(
                "AHAND_HUB_WEBHOOK_SECRET must be set when AHAND_HUB_WEBHOOK_URL is configured"
            ));
        }
        Ok(config)
    }
}

fn default_audit_fallback_path() -> PathBuf {
    std::env::temp_dir().join("ahand-hub-audit-fallback.jsonl")
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
    use std::path::PathBuf;

    use super::Config;
    use super::StoreConfig;
    use super::default_audit_fallback_path;

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
        assert_eq!(config.device_staleness_probe_interval_ms, 30_000);
        assert_eq!(config.device_staleness_timeout_ms, 180_000);
        assert_eq!(config.device_expected_heartbeat_secs, 60);
        assert_eq!(config.device_presence_ttl_secs, 60);
        assert_eq!(config.device_presence_refresh_ms, 20_000);
        assert_eq!(config.job_timeout_grace_ms, 1_000);
        assert_eq!(config.device_disconnect_grace_ms, 10 * 60 * 1_000);
        assert_eq!(config.jwt_secret, "jwt-prod-secret");
        assert_eq!(config.audit_retention_days, 90);
        assert!(config.webhook_url.is_none());
        assert!(config.webhook_secret.is_none());
        assert_eq!(config.webhook_max_retries, 8);
        assert_eq!(config.webhook_max_concurrency, 50);
        assert_eq!(config.audit_fallback_path, default_audit_fallback_path());
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

    #[test]
    fn from_env_with_parses_audit_retention_and_fallback_path() {
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
                "AHAND_HUB_AUDIT_RETENTION_DAYS".to_string(),
                "45".to_string(),
            ),
            (
                "AHAND_HUB_AUDIT_FALLBACK_PATH".to_string(),
                "/var/lib/ahand-hub/audit-fallback.jsonl".to_string(),
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
        assert_eq!(config.audit_retention_days, 45);
        assert_eq!(
            config.audit_fallback_path,
            PathBuf::from("/var/lib/ahand-hub/audit-fallback.jsonl")
        );
    }

    fn base_required_env() -> HashMap<String, String> {
        HashMap::from([
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
        ])
    }

    #[test]
    fn from_env_with_parses_webhook_tuning() {
        let mut env = base_required_env();
        env.insert(
            "AHAND_HUB_WEBHOOK_URL".to_string(),
            "https://gateway.example/webhook".to_string(),
        );
        env.insert(
            "AHAND_HUB_WEBHOOK_SECRET".to_string(),
            "webhook-secret-value".to_string(),
        );
        env.insert("AHAND_HUB_WEBHOOK_MAX_RETRIES".to_string(), "3".to_string());
        env.insert(
            "AHAND_HUB_WEBHOOK_MAX_CONCURRENCY".to_string(),
            "12".to_string(),
        );

        let config = Config::from_env_with(|key| env.get(key).cloned()).unwrap();
        assert_eq!(
            config.webhook_url.as_deref(),
            Some("https://gateway.example/webhook")
        );
        assert_eq!(config.webhook_secret.as_deref(), Some("webhook-secret-value"));
        assert_eq!(config.webhook_max_retries, 3);
        assert_eq!(config.webhook_max_concurrency, 12);
    }

    #[test]
    fn from_env_with_rejects_webhook_url_without_secret() {
        let mut env = base_required_env();
        env.insert(
            "AHAND_HUB_WEBHOOK_URL".to_string(),
            "https://gateway.example/webhook".to_string(),
        );
        let err = Config::from_env_with(|key| env.get(key).cloned()).unwrap_err();
        assert!(
            err.to_string().contains("AHAND_HUB_WEBHOOK_SECRET"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn from_env_with_treats_blank_webhook_url_as_unset() {
        let mut env = base_required_env();
        env.insert("AHAND_HUB_WEBHOOK_URL".to_string(), "   ".to_string());
        let config = Config::from_env_with(|key| env.get(key).cloned()).unwrap();
        assert!(config.webhook_url.is_none());
    }
}
