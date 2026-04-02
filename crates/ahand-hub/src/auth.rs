use ahand_hub_core::auth::{AuthContext, Role};
use ahand_hub_core::{HubError, Result as HubResult};
use ahand_protocol::{Hello, hello};
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{StatusCode, header::AUTHORIZATION};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct VerifiedDeviceHello {
    pub public_key: Vec<u8>,
    pub signed_at_ms: u64,
    pub auth_method: &'static str,
    pub allow_registration: bool,
}

pub struct AuthContextExt(pub AuthContext);

impl AuthContextExt {
    pub fn require_dashboard_access(&self) -> Result<(), StatusCode> {
        match self.0.role {
            Role::Admin | Role::DashboardUser => Ok(()),
            _ => Err(StatusCode::FORBIDDEN),
        }
    }

    pub fn require_admin(&self) -> Result<(), StatusCode> {
        if self.0.role == Role::Admin {
            Ok(())
        } else {
            Err(StatusCode::FORBIDDEN)
        }
    }

    pub fn require_read_devices(&self) -> Result<(), StatusCode> {
        self.require_dashboard_access()
    }

    pub fn require_read_device(&self, device_id: &str) -> Result<(), StatusCode> {
        match self.0.role {
            Role::Admin | Role::DashboardUser => Ok(()),
            Role::Device if self.0.subject == device_id => Ok(()),
            _ => Err(StatusCode::FORBIDDEN),
        }
    }

    pub fn require_read_jobs(&self) -> Result<(), StatusCode> {
        self.require_dashboard_access()
    }

    pub fn require_read_audit(&self) -> Result<(), StatusCode> {
        self.require_dashboard_access()
    }

    pub fn require_read_stats(&self) -> Result<(), StatusCode> {
        self.require_dashboard_access()
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

        authenticate_token(state, token)
            .map(Self)
            .map_err(|_| StatusCode::UNAUTHORIZED)
    }
}

pub fn authenticate_token(state: &AppState, token: &str) -> HubResult<AuthContext> {
    if token == state.service_token.as_str() {
        return Ok(AuthContext {
            role: Role::Admin,
            subject: "service".into(),
            iss: "ahand-hub".into(),
            exp: usize::MAX,
        });
    }

    state.auth.verify_jwt(token)
}

pub fn verify_device_hello(
    device_id: &str,
    hello: &Hello,
    auth_service: &ahand_hub_core::auth::AuthService,
    challenge_nonce: &[u8],
    bootstrap_token: &str,
    bootstrap_device_id: &str,
    max_age_ms: u64,
) -> HubResult<VerifiedDeviceHello> {
    let Some(auth) = hello.auth.as_ref() else {
        return Err(HubError::Unauthorized);
    };

    match auth {
        hello::Auth::Ed25519(auth) => {
            let public_key = verify_signed_auth(
                device_id,
                hello,
                &auth.public_key,
                &auth.signature,
                auth.signed_at_ms,
                challenge_nonce,
                max_age_ms,
            )?;
            Ok(VerifiedDeviceHello {
                public_key,
                signed_at_ms: auth.signed_at_ms,
                auth_method: "ed25519",
                allow_registration: false,
            })
        }
        hello::Auth::Bootstrap(auth) => {
            if auth.bearer_token == bootstrap_token && device_id == bootstrap_device_id {
                let public_key = verify_signed_auth(
                    device_id,
                    hello,
                    &auth.public_key,
                    &auth.signature,
                    auth.signed_at_ms,
                    challenge_nonce,
                    max_age_ms,
                )?;
                Ok(VerifiedDeviceHello {
                    public_key,
                    signed_at_ms: auth.signed_at_ms,
                    auth_method: "bootstrap",
                    allow_registration: true,
                })
            } else {
                let claims = auth_service.verify_jwt(&auth.bearer_token)?;
                if claims.role != Role::Device || claims.subject != device_id {
                    return Err(HubError::Unauthorized);
                }
                let public_key = verify_signed_auth(
                    device_id,
                    hello,
                    &auth.public_key,
                    &auth.signature,
                    auth.signed_at_ms,
                    challenge_nonce,
                    max_age_ms,
                )?;
                Ok(VerifiedDeviceHello {
                    public_key,
                    signed_at_ms: auth.signed_at_ms,
                    auth_method: "bootstrap",
                    allow_registration: true,
                })
            }
        }
    }
}

fn verify_signed_auth(
    device_id: &str,
    hello: &Hello,
    public_key: &[u8],
    signature: &[u8],
    signed_at_ms: u64,
    challenge_nonce: &[u8],
    max_age_ms: u64,
) -> HubResult<Vec<u8>> {
    validate_signed_at_ms(signed_at_ms, max_age_ms)?;

    let public_key: [u8; 32] = public_key
        .try_into()
        .map_err(|_| HubError::InvalidSignature)?;
    let signature: [u8; 64] = signature
        .try_into()
        .map_err(|_| HubError::InvalidSignature)?;
    let verifying_key =
        VerifyingKey::from_bytes(&public_key).map_err(|_| HubError::InvalidSignature)?;
    let signature = Signature::from_bytes(&signature);
    let payload =
        ahand_protocol::build_hello_auth_payload(device_id, hello, signed_at_ms, challenge_nonce);
    verifying_key
        .verify(&payload, &signature)
        .map_err(|_| HubError::InvalidSignature)?;

    Ok(public_key.to_vec())
}

fn validate_signed_at_ms(signed_at_ms: u64, max_age_ms: u64) -> HubResult<()> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| HubError::Unauthorized)?
        .as_millis() as u64;

    if now_ms.saturating_sub(signed_at_ms) > max_age_ms
        || signed_at_ms > now_ms.saturating_add(max_age_ms)
    {
        return Err(HubError::Unauthorized);
    }

    Ok(())
}
