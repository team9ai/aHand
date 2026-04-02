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

    pub async fn list_devices(&self) -> Result<Vec<Device>> {
        self.devices.list().await
    }
}
