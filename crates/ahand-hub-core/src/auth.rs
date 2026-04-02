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
        self.issue_jwt(Role::DashboardUser, subject)
    }

    pub fn issue_device_jwt(&self, subject: &str) -> Result<String> {
        self.issue_jwt(Role::Device, subject)
    }

    pub fn verify_jwt(&self, token: &str) -> Result<AuthContext> {
        decode::<AuthContext>(token, &self.decoding_key, &Validation::default())
            .map(|data| data.claims)
            .map_err(|err| HubError::InvalidToken(err.to_string()))
    }

    fn issue_jwt(&self, role: Role, subject: &str) -> Result<String> {
        let claims = AuthContext {
            role,
            subject: subject.into(),
            iss: "ahand-hub".into(),
            exp: (Utc::now() + Duration::hours(24)).timestamp() as usize,
        };

        let token = encode(&Header::default(), &claims, &self.encoding_key)
            .expect("AuthContext should always serialize into a JWT");
        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthService, Role};

    #[test]
    fn issue_jwt_supports_admin_claims() {
        let service = AuthService::new_for_tests("unit-test-secret");
        let token = service.issue_jwt(Role::Admin, "service:test").unwrap();
        let claims = service.verify_jwt(&token).unwrap();

        assert_eq!(claims.role, Role::Admin);
        assert_eq!(claims.subject, "service:test");
        assert_eq!(claims.iss, "ahand-hub");
    }
}
