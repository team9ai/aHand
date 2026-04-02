use ahand_hub_core::HubError;
use ahand_hub_core::auth::{AuthContext, AuthService, Role};
use chrono::{Duration, Utc};
use jsonwebtoken::{EncodingKey, Header, encode};

#[test]
fn dashboard_jwt_roundtrip_preserves_role() {
    let service = AuthService::new_for_tests("unit-test-secret");
    let token = service.issue_dashboard_jwt("operator-1").unwrap();
    let claims = service.verify_jwt(&token).unwrap();

    assert_eq!(claims.role, Role::DashboardUser);
    assert_eq!(claims.subject, "operator-1");
}

#[test]
fn device_jwt_roundtrip_preserves_role() {
    let service = AuthService::new_for_tests("unit-test-secret");
    let token = service.issue_device_jwt("device-7").unwrap();
    let claims = service.verify_jwt(&token).unwrap();

    assert_eq!(claims.role, Role::Device);
    assert_eq!(claims.subject, "device-7");
    assert_eq!(claims.iss, "ahand-hub");
    assert!(claims.exp > 0);
}

#[test]
fn verify_jwt_rejects_invalid_tokens() {
    let service = AuthService::new_for_tests("unit-test-secret");
    let err = service.verify_jwt("not-a-jwt").unwrap_err();

    assert!(matches!(err, HubError::InvalidToken(_)));
}

#[test]
fn verify_jwt_rejects_tokens_with_the_wrong_issuer() {
    let service = AuthService::new_for_tests("unit-test-secret");
    let token = encode(
        &Header::default(),
        &AuthContext {
            role: Role::Device,
            subject: "device-7".into(),
            iss: "wrong-issuer".into(),
            exp: (Utc::now() + Duration::hours(24)).timestamp() as usize,
        },
        &EncodingKey::from_secret("unit-test-secret".as_bytes()),
    )
    .unwrap();

    let err = service.verify_jwt(&token).unwrap_err();

    assert!(matches!(err, HubError::InvalidToken(_)));
}
