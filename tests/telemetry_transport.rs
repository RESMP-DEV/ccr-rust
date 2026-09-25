#![cfg(feature = "telemetry")]
use opentelemetry::trace::{Span, Tracer, TracerProvider};
use std::time::{Duration, Instant};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn production_exporter_does_not_follow_redirects() {
    let destination = MockServer::start().await;
    let collector = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/traces"))
        .respond_with(ResponseTemplate::new(307).insert_header("Location", destination.uri()))
        .mount(&collector)
        .await;
    let endpoint = format!("{}/v1/traces", collector.uri());
    let provider = tokio::task::spawn_blocking(move || {
        ccr_rust::telemetry::provider_for_endpoint(&endpoint).unwrap()
    })
    .await
    .unwrap();
    provider.tracer("redirect-test").start("canary").end();
    tokio::task::spawn_blocking(move || {
        let _ = provider.force_flush();
        let _ = provider.shutdown();
    })
    .await
    .unwrap();
    let received = collector.received_requests().await.unwrap();
    assert!(!received.is_empty());
    assert!(received
        .iter()
        .any(|request| request.url.path() == "/v1/traces"));
    assert!(destination.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn slow_exporter_and_queue_overflow_do_not_block_span_production() {
    let collector = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/traces"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(10)))
        .mount(&collector)
        .await;
    let endpoint = format!("{}/v1/traces", collector.uri());
    let provider = tokio::task::spawn_blocking(move || {
        ccr_rust::telemetry::provider_for_endpoint(&endpoint).unwrap()
    })
    .await
    .unwrap();
    let tracer = provider.tracer("overflow-test");
    let start = Instant::now();
    for _ in 0..4096 {
        tracer.start("canary").end();
    }
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "exporter blocked the caller"
    );
    tokio::time::timeout(
        Duration::from_secs(15),
        tokio::task::spawn_blocking(move || {
            let _ = provider.shutdown_with_timeout(Duration::from_secs(10));
        }),
    )
    .await
    .unwrap()
    .unwrap();
}
