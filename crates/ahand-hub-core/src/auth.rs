use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
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

/// Discriminator so `verify_*_jwt` can refuse a token minted for the
/// wrong surface. Control-plane tokens must never validate as device
/// tokens and vice versa.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TokenType {
    Device,
    ControlPlane,
}

/// Device JWT: granted to a specific device, scoped by `external_user_id`.
/// The `sub` is the device id (convention aligned with the existing
/// `AuthContext` shape so downstream consumers can keep using `sub` as
/// the resource id).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceJwtClaims {
    pub iss: String,
    pub sub: String,
    pub external_user_id: String,
    pub exp: i64,
    pub iat: i64,
    pub token_type: TokenType,
}

/// Control-plane JWT: granted to a user, optionally scoped to a
/// specific set of device ids. Used by the team9 agent to execute
/// jobs on behalf of the user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlPlaneJwtClaims {
    pub iss: String,
    pub sub: String,
    pub external_user_id: String,
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_ids: Option<Vec<String>>,
    pub exp: i64,
    pub iat: i64,
    pub token_type: TokenType,
}

/// 24 hours — default device JWT TTL.
pub const DEVICE_JWT_DEFAULT_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// 7 days — hard cap on device JWT TTL.
pub const DEVICE_JWT_MAX_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// 1 hour — default AND hard cap on control-plane JWT TTL.
pub const CONTROL_PLANE_JWT_DEFAULT_TTL: Duration = Duration::from_secs(60 * 60);
pub const CONTROL_PLANE_JWT_MAX_TTL: Duration = CONTROL_PLANE_JWT_DEFAULT_TTL;

#[derive(Clone)]
pub struct AuthService {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    secret: Vec<u8>,
}

impl AuthService {
    pub fn new(secret: &str) -> Self {
        Self {
            encoding_key: EncodingKey::from_secret(secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(secret.as_bytes()),
            secret: secret.as_bytes().to_vec(),
        }
    }

    pub fn issue_dashboard_jwt(&self, subject: &str) -> Result<String> {
        self.issue_jwt(Role::DashboardUser, subject)
    }

    pub fn issue_device_jwt(&self, subject: &str) -> Result<String> {
        self.issue_jwt(Role::Device, subject)
    }

    pub fn verify_jwt(&self, token: &str) -> Result<AuthContext> {
        let mut validation = Validation::default();
        validation.set_issuer(&["ahand-hub"]);
        validation.set_required_spec_claims(&["exp", "iss"]);

        decode::<AuthContext>(token, &self.decoding_key, &validation)
            .map(|data| data.claims)
            .map_err(|err| HubError::InvalidToken(err.to_string()))
    }

    /// Mint a device-scoped JWT. `ttl` is clamped to [`DEVICE_JWT_MAX_TTL`];
    /// callers that pass `Duration::ZERO` or anything longer than the cap
    /// get the clamp. Returns `(token, expires_at)`.
    pub fn mint_device_jwt_with_external_user(
        &self,
        device_id: &str,
        external_user_id: &str,
        ttl: Duration,
    ) -> Result<(String, DateTime<Utc>)> {
        mint_device_jwt(&self.secret, device_id, external_user_id, ttl)
    }

    pub fn mint_control_plane_jwt(
        &self,
        external_user_id: &str,
        scope: &str,
        device_ids: Option<Vec<String>>,
        ttl: Duration,
    ) -> Result<(String, DateTime<Utc>)> {
        mint_control_plane_jwt(&self.secret, external_user_id, scope, device_ids, ttl)
    }

    pub fn verify_device_jwt(&self, token: &str) -> Result<DeviceJwtClaims> {
        verify_device_jwt(&self.secret, token)
    }

    pub fn verify_control_plane_jwt(&self, token: &str) -> Result<ControlPlaneJwtClaims> {
        verify_control_plane_jwt(&self.secret, token)
    }

    fn issue_jwt(&self, role: Role, subject: &str) -> Result<String> {
        let claims = AuthContext {
            role,
            subject: subject.into(),
            iss: "ahand-hub".into(),
            exp: (Utc::now() + ChronoDuration::hours(24)).timestamp() as usize,
        };

        let token = encode(&Header::default(), &claims, &self.encoding_key)
            .expect("AuthContext should always serialize into a JWT");
        Ok(token)
    }
}

/// Free-function form of [`AuthService::mint_device_jwt_with_external_user`]
/// so callers that hold a raw secret (e.g. the admin router) don't need to
/// re-wrap it.
pub fn mint_device_jwt(
    secret: &[u8],
    device_id: &str,
    external_user_id: &str,
    ttl: Duration,
) -> Result<(String, DateTime<Utc>)> {
    let ttl = clamp_ttl(ttl, DEVICE_JWT_DEFAULT_TTL, DEVICE_JWT_MAX_TTL);
    let now = Utc::now();
    let expires_at = now + ChronoDuration::from_std(ttl).expect("ttl clamped to a sane range");
    let claims = DeviceJwtClaims {
        iss: "ahand-hub".into(),
        sub: device_id.into(),
        external_user_id: external_user_id.into(),
        exp: expires_at.timestamp(),
        iat: now.timestamp(),
        token_type: TokenType::Device,
    };
    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret),
    )
    .map_err(|err| HubError::Internal(format!("failed to encode device jwt: {err}")))?;
    Ok((token, expires_at))
}

pub fn mint_control_plane_jwt(
    secret: &[u8],
    external_user_id: &str,
    scope: &str,
    device_ids: Option<Vec<String>>,
    ttl: Duration,
) -> Result<(String, DateTime<Utc>)> {
    let ttl = clamp_ttl(
        ttl,
        CONTROL_PLANE_JWT_DEFAULT_TTL,
        CONTROL_PLANE_JWT_MAX_TTL,
    );
    let now = Utc::now();
    let expires_at = now + ChronoDuration::from_std(ttl).expect("ttl clamped to a sane range");
    let claims = ControlPlaneJwtClaims {
        iss: "ahand-hub".into(),
        sub: external_user_id.into(),
        external_user_id: external_user_id.into(),
        scope: scope.into(),
        device_ids,
        exp: expires_at.timestamp(),
        iat: now.timestamp(),
        token_type: TokenType::ControlPlane,
    };
    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret),
    )
    .map_err(|err| HubError::Internal(format!("failed to encode control-plane jwt: {err}")))?;
    Ok((token, expires_at))
}

pub fn verify_device_jwt(secret: &[u8], token: &str) -> Result<DeviceJwtClaims> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_issuer(&["ahand-hub"]);
    validation.set_required_spec_claims(&["exp", "iss"]);
    let claims = decode::<DeviceJwtClaims>(token, &DecodingKey::from_secret(secret), &validation)
        .map(|data| data.claims)
        .map_err(|err| HubError::InvalidToken(err.to_string()))?;
    if claims.token_type != TokenType::Device {
        return Err(HubError::InvalidToken("unexpected token_type".into()));
    }
    Ok(claims)
}

pub fn verify_control_plane_jwt(secret: &[u8], token: &str) -> Result<ControlPlaneJwtClaims> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_issuer(&["ahand-hub"]);
    validation.set_required_spec_claims(&["exp", "iss"]);
    let claims =
        decode::<ControlPlaneJwtClaims>(token, &DecodingKey::from_secret(secret), &validation)
            .map(|data| data.claims)
            .map_err(|err| HubError::InvalidToken(err.to_string()))?;
    if claims.token_type != TokenType::ControlPlane {
        return Err(HubError::InvalidToken("unexpected token_type".into()));
    }
    Ok(claims)
}

/// If `ttl == 0`, use `default`; otherwise cap at `max`. A zero TTL is
/// treated as "unspecified" rather than "expire immediately" because that
/// matches the admin API's `ttl_seconds: Option<u64>` ergonomics — an
/// absent value and a zero value both mean "use the default".
fn clamp_ttl(ttl: Duration, default: Duration, max: Duration) -> Duration {
    if ttl.is_zero() {
        return default;
    }
    ttl.min(max)
}

#[cfg(test)]
mod tests {
    use super::{
        AuthService, CONTROL_PLANE_JWT_DEFAULT_TTL, CONTROL_PLANE_JWT_MAX_TTL,
        DEVICE_JWT_DEFAULT_TTL, DEVICE_JWT_MAX_TTL, Role, TokenType, clamp_ttl,
        mint_control_plane_jwt, mint_device_jwt, verify_control_plane_jwt, verify_device_jwt,
    };
    use crate::HubError;
    use std::time::Duration;

    const SECRET: &[u8] = b"unit-test-secret";

    #[test]
    fn issue_jwt_supports_admin_claims() {
        let service = AuthService::new("unit-test-secret");
        let token = service.issue_jwt(Role::Admin, "service:test").unwrap();
        let claims = service.verify_jwt(&token).unwrap();

        assert_eq!(claims.role, Role::Admin);
        assert_eq!(claims.subject, "service:test");
        assert_eq!(claims.iss, "ahand-hub");
    }

    #[test]
    fn mint_device_jwt_roundtrip_with_default_ttl() {
        let (token, expires_at) =
            mint_device_jwt(SECRET, "device-1", "user-9", Duration::ZERO).unwrap();
        let now = chrono::Utc::now();
        let claims = verify_device_jwt(SECRET, &token).unwrap();

        assert_eq!(claims.sub, "device-1");
        assert_eq!(claims.external_user_id, "user-9");
        assert_eq!(claims.token_type, TokenType::Device);
        assert_eq!(claims.iss, "ahand-hub");
        // iat within the last second, exp ~24h out
        assert!((claims.iat - now.timestamp()).abs() <= 2);
        let default_secs = DEVICE_JWT_DEFAULT_TTL.as_secs() as i64;
        assert!((claims.exp - now.timestamp() - default_secs).abs() <= 2);
        assert_eq!(expires_at.timestamp(), claims.exp);
    }

    #[test]
    fn mint_device_jwt_clamps_to_max_ttl() {
        let huge = Duration::from_secs(30 * 24 * 60 * 60);
        let (_, expires_at) = mint_device_jwt(SECRET, "device-1", "user-9", huge).unwrap();
        let now = chrono::Utc::now();
        let max_secs = DEVICE_JWT_MAX_TTL.as_secs() as i64;
        assert!((expires_at.timestamp() - now.timestamp() - max_secs).abs() <= 2);
    }

    #[test]
    fn mint_device_jwt_respects_explicit_ttl_within_cap() {
        let ttl = Duration::from_secs(3600);
        let (_, expires_at) = mint_device_jwt(SECRET, "device-1", "user-9", ttl).unwrap();
        let now = chrono::Utc::now();
        assert!((expires_at.timestamp() - now.timestamp() - 3600).abs() <= 2);
    }

    #[test]
    fn mint_control_plane_jwt_uses_1h_default_and_cap() {
        let (_, expires_at) =
            mint_control_plane_jwt(SECRET, "user-9", "jobs:execute", None, Duration::ZERO).unwrap();
        let now = chrono::Utc::now();
        let expected = CONTROL_PLANE_JWT_DEFAULT_TTL.as_secs() as i64;
        assert!((expires_at.timestamp() - now.timestamp() - expected).abs() <= 2);

        let (_, over_expires) = mint_control_plane_jwt(
            SECRET,
            "user-9",
            "jobs:execute",
            Some(vec!["d1".into()]),
            Duration::from_secs(24 * 60 * 60),
        )
        .unwrap();
        let max = CONTROL_PLANE_JWT_MAX_TTL.as_secs() as i64;
        assert!((over_expires.timestamp() - now.timestamp() - max).abs() <= 2);
    }

    #[test]
    fn verify_device_jwt_rejects_wrong_secret() {
        let (token, _) =
            mint_device_jwt(SECRET, "device-1", "user-9", Duration::from_secs(60)).unwrap();
        let err = verify_device_jwt(b"different-secret", &token).unwrap_err();
        assert!(matches!(err, HubError::InvalidToken(_)));
    }

    #[test]
    fn verify_device_jwt_rejects_control_plane_token() {
        let (token, _) = mint_control_plane_jwt(
            SECRET,
            "user-9",
            "jobs:execute",
            None,
            Duration::from_secs(60),
        )
        .unwrap();
        let err = verify_device_jwt(SECRET, &token).unwrap_err();
        match err {
            HubError::InvalidToken(msg) => {
                assert!(msg.contains("token_type"), "got {msg}");
            }
            other => panic!("expected InvalidToken, got {other:?}"),
        }
    }

    #[test]
    fn verify_control_plane_jwt_rejects_device_token() {
        let (token, _) =
            mint_device_jwt(SECRET, "device-1", "user-9", Duration::from_secs(60)).unwrap();
        let err = verify_control_plane_jwt(SECRET, &token).unwrap_err();
        assert!(matches!(err, HubError::InvalidToken(_)));
    }

    #[test]
    fn verify_device_jwt_rejects_expired_token() {
        // Manually forge an expired token by going through the encoder
        // with an exp well in the past (jsonwebtoken's default
        // `Validation` has a 60-second leeway, so we pick 5 minutes).
        use chrono::{Duration as ChronoDuration, Utc};
        use jsonwebtoken::{EncodingKey, Header, encode};
        let now = Utc::now();
        let exp = (now - ChronoDuration::seconds(300)).timestamp();
        let claims = super::DeviceJwtClaims {
            iss: "ahand-hub".into(),
            sub: "device-1".into(),
            external_user_id: "user-9".into(),
            exp,
            iat: (now - ChronoDuration::seconds(310)).timestamp(),
            token_type: TokenType::Device,
        };
        let token = encode(&Header::default(), &claims, &EncodingKey::from_secret(SECRET)).unwrap();
        let err = verify_device_jwt(SECRET, &token).unwrap_err();
        assert!(matches!(err, HubError::InvalidToken(_)));
    }

    #[test]
    fn verify_device_jwt_rejects_missing_iss() {
        // Hand-craft a claims struct without an `iss` field by using a
        // separate local struct that omits it.
        use chrono::Utc;
        use jsonwebtoken::{EncodingKey, Header, encode};
        #[derive(serde::Serialize)]
        struct NoIssDeviceClaims {
            sub: String,
            external_user_id: String,
            exp: i64,
            iat: i64,
            token_type: TokenType,
        }
        let now = Utc::now();
        let claims = NoIssDeviceClaims {
            sub: "device-1".into(),
            external_user_id: "user-9".into(),
            exp: (now + chrono::Duration::seconds(3600)).timestamp(),
            iat: now.timestamp(),
            token_type: TokenType::Device,
        };
        let token =
            encode(&Header::default(), &claims, &EncodingKey::from_secret(SECRET)).unwrap();
        let err = verify_device_jwt(SECRET, &token).unwrap_err();
        assert!(matches!(err, HubError::InvalidToken(_)));
    }

    #[test]
    fn verify_control_plane_jwt_rejects_missing_iss() {
        use chrono::Utc;
        use jsonwebtoken::{EncodingKey, Header, encode};
        #[derive(serde::Serialize)]
        struct NoIssCpClaims {
            sub: String,
            external_user_id: String,
            scope: String,
            exp: i64,
            iat: i64,
            token_type: TokenType,
        }
        let now = Utc::now();
        let claims = NoIssCpClaims {
            sub: "user-9".into(),
            external_user_id: "user-9".into(),
            scope: "jobs:execute".into(),
            exp: (now + chrono::Duration::seconds(3600)).timestamp(),
            iat: now.timestamp(),
            token_type: TokenType::ControlPlane,
        };
        let token =
            encode(&Header::default(), &claims, &EncodingKey::from_secret(SECRET)).unwrap();
        let err = verify_control_plane_jwt(SECRET, &token).unwrap_err();
        assert!(matches!(err, HubError::InvalidToken(_)));
    }

    #[test]
    fn auth_service_admin_wrapper_matches_free_functions() {
        let service = AuthService::new("unit-test-secret");
        let (token, _) = service
            .mint_device_jwt_with_external_user(
                "device-1",
                "user-9",
                Duration::from_secs(60),
            )
            .unwrap();
        let claims = service.verify_device_jwt(&token).unwrap();
        assert_eq!(claims.sub, "device-1");
        assert_eq!(claims.external_user_id, "user-9");

        let (cp_token, _) = service
            .mint_control_plane_jwt(
                "user-9",
                "jobs:execute",
                Some(vec!["d1".into()]),
                Duration::from_secs(60),
            )
            .unwrap();
        let cp_claims = service.verify_control_plane_jwt(&cp_token).unwrap();
        assert_eq!(cp_claims.external_user_id, "user-9");
        assert_eq!(cp_claims.device_ids.as_deref().unwrap(), &["d1".to_string()]);
    }

    #[test]
    fn clamp_ttl_uses_default_for_zero() {
        assert_eq!(
            clamp_ttl(Duration::ZERO, Duration::from_secs(5), Duration::from_secs(10)),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn clamp_ttl_caps_at_max() {
        assert_eq!(
            clamp_ttl(
                Duration::from_secs(99),
                Duration::from_secs(5),
                Duration::from_secs(10)
            ),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn clamp_ttl_passes_through_in_range() {
        assert_eq!(
            clamp_ttl(Duration::from_secs(7), Duration::from_secs(5), Duration::from_secs(10)),
            Duration::from_secs(7)
        );
    }
}
