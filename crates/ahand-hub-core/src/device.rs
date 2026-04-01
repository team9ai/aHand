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

impl Device {
    pub fn offline_for_tests(id: &str) -> Self {
        Self {
            id: id.into(),
            public_key: None,
            hostname: "offline-device".into(),
            os: "linux".into(),
            capabilities: vec!["exec".into()],
            version: Some("0.1.2".into()),
            auth_method: "ed25519".into(),
            online: false,
        }
    }
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
