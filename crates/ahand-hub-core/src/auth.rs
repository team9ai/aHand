use chrono::{Duration, Utc};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

use crate::{HubError, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Role {
    Admin,
    DashboardUser,
    Device,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthContext {
    pub role: Role,
    pub subject: String,
    pub iss: String,
    pub exp: usize,
}

#[derive(Clone)]
pub struct AuthService {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
}

impl AuthService {
    pub fn new_for_tests(secret: &str) -> Self {
        Self {
            encoding_key: EncodingKey::from_secret(secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(secret.as_bytes()),
        }
    }

    pub fn issue_dashboard_jwt(&self, subject: &str) -> Result<String> {
        let claims = AuthContext {
            role: Role::DashboardUser,
            subject: subject.into(),
            iss: "ahand-hub".into(),
            exp: (Utc::now() + Duration::hours(24)).timestamp() as usize,
        };

        encode(&Header::default(), &claims, &self.encoding_key)
            .map_err(|err| HubError::InvalidToken(err.to_string()))
    }

    pub fn verify_jwt(&self, token: &str) -> Result<AuthContext> {
        decode::<AuthContext>(token, &self.decoding_key, &Validation::default())
            .map(|data| data.claims)
            .map_err(|err| HubError::InvalidToken(err.to_string()))
    }
}
