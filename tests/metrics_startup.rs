use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use tower::ServiceExt;

#[tokio::test]
async fn fresh_process_metrics_include_idle_gauges() {
    let app = Router::new().route("/metrics", get(ccr_rust::metrics::metrics_handler));
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/metrics")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("ccr_active_requests 0"));
    assert!(body.contains("ccr_active_streams 0"));
}
