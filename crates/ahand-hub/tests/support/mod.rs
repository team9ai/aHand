use axum::body::Body;
use axum::http::{Request, header::AUTHORIZATION};

pub fn service_request(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(AUTHORIZATION, "Bearer service-test-token")
        .body(Body::empty())
        .unwrap()
}
