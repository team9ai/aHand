use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub public_key: Option<Vec<u8>>,
    pub hostname: String,
    pub os: String,
    pub capabilities: Vec<String>,
    pub version: Option<String>,
    pub auth_method: String,
    pub online: bool,
    /// Opaque identifier assigned by the team9 gateway (or any other
    /// upstream tenant) that owns this device. `None` for devices that
    /// were created via the legacy bootstrap flow and never claimed by
    /// an external user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_user_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewDevice {
    pub id: String,
    pub public_key: Option<Vec<u8>>,
    pub hostname: String,
    pub os: String,
    pub capabilities: Vec<String>,
    pub version: Option<String>,
    pub auth_method: String,
    pub external_user_id: Option<String>,
}
