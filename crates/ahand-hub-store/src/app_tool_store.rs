use std::sync::Arc;

use ahand_hub_core::{HubError, Result};
use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use tokio::sync::Mutex;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredAppToolCatalog {
    pub revision: u64,
    pub stale: bool,
    pub tools: Vec<StoredAppTool>,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredAppTool {
    pub name: String,
    pub description: String,
    pub input_schema_json: String,
    pub requires_approval: bool,
}

/// A snapshot is accepted when the stored catalog is absent, stale, or has a
/// lower revision. Equal revision on a fresh catalog is a duplicate -> ignore.
pub fn should_accept_update(
    existing: Option<&StoredAppToolCatalog>,
    incoming_revision: u64,
) -> bool {
    match existing {
        None => true,
        Some(c) if c.stale => true,
        Some(c) => incoming_revision > c.revision,
    }
}

#[derive(Clone)]
pub struct RedisAppToolStore {
    connection: Arc<Mutex<ConnectionManager>>,
}

impl RedisAppToolStore {
    pub fn new(connection: ConnectionManager) -> Self {
        Self {
            connection: Arc::new(Mutex::new(connection)),
        }
    }

    pub async fn put_catalog(&self, device_id: &str, catalog: StoredAppToolCatalog) -> Result<()> {
        let key = app_tool_key(device_id);
        let value =
            serde_json::to_string(&catalog).map_err(|e| HubError::Internal(e.to_string()))?;
        let mut conn = self.connection.lock().await;
        let _: () = conn
            .set(key, value)
            .await
            .map_err(|e| HubError::Internal(e.to_string()))?;
        Ok(())
    }

    pub async fn get_catalog(&self, device_id: &str) -> Result<Option<StoredAppToolCatalog>> {
        let key = app_tool_key(device_id);
        let mut conn = self.connection.lock().await;
        let raw: Option<String> = conn
            .get(key)
            .await
            .map_err(|e| HubError::Internal(e.to_string()))?;
        match raw {
            None => Ok(None),
            Some(s) => {
                let catalog: StoredAppToolCatalog =
                    serde_json::from_str(&s).map_err(|e| HubError::Internal(e.to_string()))?;
                Ok(Some(catalog))
            }
        }
    }

    /// Mark the catalog stale (read-modify-write). Returns `true` if a catalog
    /// was found and updated; `false` if no catalog exists (no-op).
    pub async fn mark_stale(&self, device_id: &str) -> Result<bool> {
        let key = app_tool_key(device_id);
        let mut conn = self.connection.lock().await;
        let raw: Option<String> = conn
            .get(&key)
            .await
            .map_err(|e| HubError::Internal(e.to_string()))?;
        let Some(s) = raw else {
            return Ok(false);
        };
        let mut catalog: StoredAppToolCatalog =
            serde_json::from_str(&s).map_err(|e| HubError::Internal(e.to_string()))?;
        if catalog.stale {
            return Ok(true); // already stale, nothing to write
        }
        catalog.stale = true;
        let new_value =
            serde_json::to_string(&catalog).map_err(|e| HubError::Internal(e.to_string()))?;
        let _: () = conn
            .set(&key, new_value)
            .await
            .map_err(|e| HubError::Internal(e.to_string()))?;
        Ok(true)
    }
}

fn app_tool_key(device_id: &str) -> String {
    format!("ahand:hub:app-tools:{device_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unit tests for should_accept_update (pure function, no Redis needed).
    #[test]
    fn accept_when_no_catalog() {
        assert!(should_accept_update(None, 1));
        assert!(should_accept_update(None, 0));
    }

    #[test]
    fn accept_when_stale_regardless_of_revision() {
        let stale = StoredAppToolCatalog {
            revision: 5,
            stale: true,
            tools: vec![],
            updated_at_ms: 0,
        };
        // Same revision accepted because stale
        assert!(should_accept_update(Some(&stale), 5));
        // Lower revision accepted because stale
        assert!(should_accept_update(Some(&stale), 3));
        // Higher revision also accepted
        assert!(should_accept_update(Some(&stale), 10));
    }

    #[test]
    fn accept_only_higher_revision_when_fresh() {
        let fresh = StoredAppToolCatalog {
            revision: 5,
            stale: false,
            tools: vec![],
            updated_at_ms: 0,
        };
        // Lower revision rejected
        assert!(!should_accept_update(Some(&fresh), 4));
        // Equal revision rejected (duplicate)
        assert!(!should_accept_update(Some(&fresh), 5));
        // Higher revision accepted
        assert!(should_accept_update(Some(&fresh), 6));
    }
}
