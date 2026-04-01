use ahand_hub_core::auth::{AuthService, Role};

#[test]
fn dashboard_jwt_roundtrip_preserves_role() {
    let service = AuthService::new_for_tests("unit-test-secret");
    let token = service.issue_dashboard_jwt("operator-1").unwrap();
    let claims = service.verify_jwt(&token).unwrap();

    assert_eq!(claims.role, Role::DashboardUser);
    assert_eq!(claims.subject, "operator-1");
}
