use axum::{Router, routing::get};

use crate::state::AppState;

pub mod devices;
pub mod system;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(system::health))
        .route("/api/devices", get(devices::list_devices))
        .with_state(state)
}
