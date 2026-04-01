use ahand_hub_core::auth::{AuthContext, Role};
use ahand_hub_core::{HubError, Result as HubResult};
use ahand_protocol::{Hello, hello};
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{StatusCode, header::AUTHORIZATION};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use crate::state::AppState;

pub struct AuthContextExt(pub AuthContext);

impl AuthContextExt {
    pub fn require_admin(&self) -> Result<(), StatusCode> {
        if self.0.role == Role::Admin {
            Ok(())
        } else {
            Err(StatusCode::FORBIDDEN)
        }
    }

    pub fn require_read_devices(&self) -> Result<(), StatusCode> {
        match self.0.role {
            Role::Admin | Role::DashboardUser => Ok(()),
            _ => Err(StatusCode::FORBIDDEN),
        }
    }

    pub fn require_read_jobs(&self) -> Result<(), StatusCode> {
        match self.0.role {
            Role::Admin | Role::DashboardUser => Ok(()),
            _ => Err(StatusCode::FORBIDDEN),
        }
    }
}

impl FromRequestParts<AppState> for AuthContextExt {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Some(value) = parts.headers.get(AUTHORIZATION) else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        let Ok(value) = value.to_str() else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        let Some(token) = value.strip_prefix("Bearer ") else {
            return Err(StatusCode::UNAUTHORIZED);
        };

        if token == state.service_token.as_str() {
            return Ok(Self(AuthContext {
                role: Role::Admin,
                subject: "service:test".into(),
                iss: "ahand-hub".into(),
                exp: usize::MAX,
            }));
        }

        let claims = state
            .auth
            .verify_jwt(token)
            .map_err(|_| StatusCode::UNAUTHORIZED)?;
        Ok(Self(claims))
    }
}

pub fn verify_device_hello(
    device_id: &str,
    hello: &Hello,
    bootstrap_token: &str,
) -> HubResult<()> {
    let Some(auth) = hello.auth.as_ref() else {
        return Err(HubError::Unauthorized);
    };

    match auth {
        hello::Auth::Ed25519(auth) => {
            let public_key: [u8; 32] = auth
                .public_key
                .clone()
                .try_into()
                .map_err(|_| HubError::InvalidSignature)?;
            let signature: [u8; 64] = auth
                .signature
                .clone()
                .try_into()
                .map_err(|_| HubError::InvalidSignature)?;
            let verifying_key =
                VerifyingKey::from_bytes(&public_key).map_err(|_| HubError::InvalidSignature)?;
            let signature = Signature::from_bytes(&signature);
            let payload = format!("ahand-hub|{device_id}|{}", auth.signed_at_ms);
            verifying_key
                .verify(payload.as_bytes(), &signature)
                .map_err(|_| HubError::InvalidSignature)
        }
        hello::Auth::BearerToken(token) => {
            if token == bootstrap_token {
                Ok(())
            } else {
                Err(HubError::Unauthorized)
            }
        }
    }
}
