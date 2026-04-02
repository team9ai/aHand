use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ahand_hub_core::{HubError, Result};
use ahand_hub_store::bootstrap_store::{RedisBootstrapReservation, RedisBootstrapStore};
use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapReservation {
    pub token: String,
    pub device_id: String,
    pub reservation_id: String,
}

#[derive(Clone)]
pub struct BootstrapCredentials {
    inner: BootstrapCredentialsInner,
}

#[derive(Clone)]
enum BootstrapCredentialsInner {
    Memory(Arc<Mutex<MemoryBootstrapState>>),
    Redis(RedisBootstrapStore),
}

#[derive(Default)]
struct MemoryBootstrapState {
    by_token: HashMap<String, MemoryBootstrapToken>,
    by_device: HashMap<String, String>,
}

struct MemoryBootstrapToken {
    device_id: String,
    reservation_id: Option<String>,
}

impl BootstrapCredentials {
    pub fn memory() -> Self {
        Self {
            inner: BootstrapCredentialsInner::Memory(Arc::new(Mutex::new(
                MemoryBootstrapState::default(),
            ))),
        }
    }

    pub fn redis(store: RedisBootstrapStore) -> Self {
        Self {
            inner: BootstrapCredentialsInner::Redis(store),
        }
    }

    pub async fn issue(&self, device_id: &str) -> Result<String> {
        match &self.inner {
            BootstrapCredentialsInner::Memory(state) => {
                let mut state = state.lock().await;
                if let Some(existing_token) = state.by_device.remove(device_id) {
                    state.by_token.remove(&existing_token);
                }

                for _ in 0..4 {
                    let token = uuid::Uuid::new_v4().simple().to_string();
                    if state.by_token.contains_key(&token) {
                        continue;
                    }
                    state.by_device.insert(device_id.into(), token.clone());
                    state.by_token.insert(
                        token.clone(),
                        MemoryBootstrapToken {
                            device_id: device_id.into(),
                            reservation_id: None,
                        },
                    );
                    return Ok(token);
                }

                Err(HubError::Internal(
                    "failed to allocate unique bootstrap token".into(),
                ))
            }
            BootstrapCredentialsInner::Redis(store) => store.issue(device_id).await,
        }
    }

    pub async fn reserve(
        &self,
        device_id: &str,
        token: &str,
    ) -> Result<Option<BootstrapReservation>> {
        match &self.inner {
            BootstrapCredentialsInner::Memory(state) => {
                let mut state = state.lock().await;
                let Some(entry) = state.by_token.get_mut(token) else {
                    return Ok(None);
                };
                if entry.device_id != device_id || entry.reservation_id.is_some() {
                    return Ok(None);
                }

                let reservation_id = uuid::Uuid::new_v4().simple().to_string();
                entry.reservation_id = Some(reservation_id.clone());
                Ok(Some(BootstrapReservation {
                    token: token.into(),
                    device_id: device_id.into(),
                    reservation_id,
                }))
            }
            BootstrapCredentialsInner::Redis(store) => store
                .reserve(device_id, token)
                .await
                .map(|reservation| reservation.map(Into::into)),
        }
    }

    pub async fn release(&self, reservation: &BootstrapReservation) -> Result<()> {
        match &self.inner {
            BootstrapCredentialsInner::Memory(state) => {
                let mut state = state.lock().await;
                if let Some(entry) = state.by_token.get_mut(&reservation.token)
                    && entry.reservation_id.as_deref() == Some(reservation.reservation_id.as_str())
                {
                    entry.reservation_id = None;
                }
                Ok(())
            }
            BootstrapCredentialsInner::Redis(store) => {
                store.release(&reservation.clone().into()).await
            }
        }
    }

    pub async fn consume(&self, reservation: &BootstrapReservation) -> Result<()> {
        match &self.inner {
            BootstrapCredentialsInner::Memory(state) => {
                let mut state = state.lock().await;
                let should_remove = state
                    .by_token
                    .get(&reservation.token)
                    .map(|entry| {
                        entry.device_id == reservation.device_id
                            && entry.reservation_id.as_deref()
                                == Some(reservation.reservation_id.as_str())
                    })
                    .unwrap_or(false);
                if should_remove {
                    state.by_token.remove(&reservation.token);
                    state.by_device.remove(&reservation.device_id);
                }
                Ok(())
            }
            BootstrapCredentialsInner::Redis(store) => {
                store.consume(&reservation.clone().into()).await
            }
        }
    }

    pub async fn delete_device(&self, device_id: &str) -> Result<()> {
        match &self.inner {
            BootstrapCredentialsInner::Memory(state) => {
                let mut state = state.lock().await;
                if let Some(token) = state.by_device.remove(device_id) {
                    state.by_token.remove(&token);
                }
                Ok(())
            }
            BootstrapCredentialsInner::Redis(store) => store.delete_device(device_id).await,
        }
    }

    pub fn reservation_ttl(max_age_ms: u64) -> Duration {
        Duration::from_millis(max_age_ms.max(30_000))
    }
}

impl From<RedisBootstrapReservation> for BootstrapReservation {
    fn from(value: RedisBootstrapReservation) -> Self {
        Self {
            token: value.token,
            device_id: value.device_id,
            reservation_id: value.reservation_id,
        }
    }
}

impl From<BootstrapReservation> for RedisBootstrapReservation {
    fn from(value: BootstrapReservation) -> Self {
        Self {
            token: value.token,
            device_id: value.device_id,
            reservation_id: value.reservation_id,
        }
    }
}
