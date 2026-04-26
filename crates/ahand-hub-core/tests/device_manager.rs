use std::sync::Arc;

use ahand_hub_core::HubError;
use ahand_hub_core::device::{Device, NewDevice};
use ahand_hub_core::services::device_manager::DeviceManager;
use ahand_hub_core::traits::DeviceStore;
use async_trait::async_trait;

struct FixedDeviceStore {
    devices: Vec<Device>,
}

#[async_trait]
impl DeviceStore for FixedDeviceStore {
    async fn insert(&self, _device: NewDevice) -> ahand_hub_core::Result<Device> {
        Ok(self.devices[0].clone())
    }

    async fn get(&self, device_id: &str) -> ahand_hub_core::Result<Option<Device>> {
        Ok(self
            .devices
            .iter()
            .find(|device| device.id == device_id)
            .cloned())
    }

    async fn list(&self) -> ahand_hub_core::Result<Vec<Device>> {
        Ok(self.devices.clone())
    }

    async fn delete(&self, _device_id: &str) -> ahand_hub_core::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn list_devices_returns_store_snapshot() {
    let manager = DeviceManager::new(Arc::new(FixedDeviceStore {
        devices: vec![Device {
            id: "device-1".into(),
            public_key: None,
            hostname: "offline-device".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into()],
            version: Some("0.1.2".into()),
            auth_method: "ed25519".into(),
            online: false,
        }],
    }));

    let devices = manager.list_devices().await.unwrap();

    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].id, "device-1");
    assert!(!devices[0].online);
}

#[tokio::test]
async fn list_devices_uses_injected_store() {
    let manager = DeviceManager::new(Arc::new(FixedDeviceStore {
        devices: vec![Device {
            id: "device-9".into(),
            public_key: Some(vec![9; 32]),
            hostname: "prod-box".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into(), "browser".into()],
            version: Some("0.2.0".into()),
            auth_method: "ed25519".into(),
            online: true,
        }],
    }));

    let devices = manager.list_devices().await.unwrap();

    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].id, "device-9");
    assert!(devices[0].online);
}

struct ErrorDeviceStore;

#[async_trait]
impl DeviceStore for ErrorDeviceStore {
    async fn insert(&self, _device: NewDevice) -> ahand_hub_core::Result<Device> {
        Err(HubError::Internal("store unavailable".into()))
    }

    async fn get(&self, _device_id: &str) -> ahand_hub_core::Result<Option<Device>> {
        Err(HubError::Internal("store unavailable".into()))
    }

    async fn list(&self) -> ahand_hub_core::Result<Vec<Device>> {
        Err(HubError::Internal("store unavailable".into()))
    }

    async fn delete(&self, _device_id: &str) -> ahand_hub_core::Result<()> {
        Err(HubError::Internal("store unavailable".into()))
    }
}

struct EmptyDeviceStore;

#[async_trait]
impl DeviceStore for EmptyDeviceStore {
    async fn insert(&self, _device: NewDevice) -> ahand_hub_core::Result<Device> {
        unreachable!()
    }

    async fn get(&self, _device_id: &str) -> ahand_hub_core::Result<Option<Device>> {
        Ok(None)
    }

    async fn list(&self) -> ahand_hub_core::Result<Vec<Device>> {
        Ok(vec![])
    }

    async fn delete(&self, _device_id: &str) -> ahand_hub_core::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn list_devices_propagates_store_errors() {
    let manager = DeviceManager::new(Arc::new(ErrorDeviceStore));

    let err = manager.list_devices().await.unwrap_err();

    assert_eq!(err, HubError::Internal("store unavailable".into()));
}

#[tokio::test]
async fn list_devices_returns_empty_vec_for_empty_store() {
    let manager = DeviceManager::new(Arc::new(EmptyDeviceStore));

    let devices = manager.list_devices().await.unwrap();

    assert!(devices.is_empty());
}
