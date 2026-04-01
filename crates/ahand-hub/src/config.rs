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
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(value) = std::env::var("AHAND_HUB_BIND_ADDR") {
            config.bind_addr = value;
        }
        if let Ok(value) = std::env::var("AHAND_HUB_SERVICE_TOKEN") {
            config.service_token = value;
        }
        if let Ok(value) = std::env::var("AHAND_HUB_DEVICE_BOOTSTRAP_TOKEN") {
            config.device_bootstrap_token = value;
        }
        if let Ok(value) = std::env::var("AHAND_HUB_DEVICE_BOOTSTRAP_DEVICE_ID") {
            config.device_bootstrap_device_id = value;
        }
        if let Ok(value) = std::env::var("AHAND_HUB_DEVICE_HELLO_MAX_AGE_MS") {
            config.device_hello_max_age_ms =
                value.parse().unwrap_or(config.device_hello_max_age_ms);
        }
        if let Ok(value) = std::env::var("AHAND_HUB_JWT_SECRET") {
            config.jwt_secret = value;
        }

        config
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:8080".into(),
            service_token: "service-test-token".into(),
            device_bootstrap_token: "bootstrap-test-token".into(),
            device_bootstrap_device_id: "device-2".into(),
            device_hello_max_age_ms: 30_000,
            jwt_secret: "service-test-secret".into(),
        }
    }
}
