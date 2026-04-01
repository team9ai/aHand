mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

#[tokio::test]
async fn health_endpoint_reports_ok() {
    let app = ahand_hub::build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn devices_endpoint_requires_auth() {
    let app = ahand_hub::build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/devices")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn devices_endpoint_accepts_service_token() {
    let app = ahand_hub::build_test_app().await;
    let response = app
        .oneshot(support::service_request("/api/devices"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}
