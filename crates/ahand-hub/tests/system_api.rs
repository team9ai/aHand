mod support;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
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

#[tokio::test]
async fn create_job_returns_conflict_for_offline_device() {
    let app = ahand_hub::build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header(header::AUTHORIZATION, "Bearer service-test-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "device_id": "device-1",
                        "tool": "echo",
                        "args": ["hello"],
                        "timeout_ms": 30_000
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn create_job_returns_not_found_for_unknown_device() {
    let app = ahand_hub::build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header(header::AUTHORIZATION, "Bearer service-test-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "device_id": "device-404",
                        "tool": "echo",
                        "args": ["hello"],
                        "timeout_ms": 30_000
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stream_output_returns_not_found_for_unknown_job() {
    let app = ahand_hub::build_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/jobs/missing-job/output")
                .header(header::AUTHORIZATION, "Bearer service-test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
