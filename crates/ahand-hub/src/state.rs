use std::sync::Arc;

use ahand_hub_core::auth::AuthService;
use ahand_hub_core::services::device_manager::DeviceManager;

#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<AuthService>,
    pub device_manager: Arc<DeviceManager>,
    pub service_token: Arc<String>,
}

impl AppState {
    pub async fn from_config(config: crate::config::Config) -> Self {
        Self {
            auth: Arc::new(AuthService::new_for_tests(&config.jwt_secret)),
            device_manager: Arc::new(DeviceManager::for_tests()),
            service_token: Arc::new(config.service_token),
        }
    }

    pub async fn for_tests() -> Self {
        Self::from_config(crate::config::Config::for_tests()).await
    }
}
