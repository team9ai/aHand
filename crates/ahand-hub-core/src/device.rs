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
}
