use std::sync::Arc;

use crate::Result;
use crate::device::Device;
use crate::traits::DeviceStore;

pub struct DeviceManager {
    devices: Arc<dyn DeviceStore>,
}

impl DeviceManager {
    pub fn new(devices: Arc<dyn DeviceStore>) -> Self {
        Self { devices }
    }

    pub fn for_tests() -> Self {
        let stores = crate::tests::fakes::offline_job_stores();
        Self {
            devices: stores.devices,
        }
    }

    pub async fn list_devices(&self) -> Result<Vec<Device>> {
        self.devices.list().await
    }
}
