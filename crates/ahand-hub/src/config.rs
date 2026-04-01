use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub bind_addr: String,
    pub service_token: String,
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
        if let Ok(value) = std::env::var("AHAND_HUB_JWT_SECRET") {
            config.jwt_secret = value;
        }

        config
    }

    pub fn for_tests() -> Self {
        Self {
            bind_addr: "127.0.0.1:0".into(),
            service_token: "service-test-token".into(),
            jwt_secret: "service-test-secret".into(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:8080".into(),
            service_token: "service-test-token".into(),
            jwt_secret: "service-test-secret".into(),
        }
    }
}
