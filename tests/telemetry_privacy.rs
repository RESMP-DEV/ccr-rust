use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    routing::{get, post},
    Router,
};
use opentelemetry::trace::{Status, TracerProvider};
use opentelemetry_sdk::{
    error::OTelSdkResult,
    trace::{SdkTracerProvider, SpanData, SpanExporter},
};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::{layer::SubscriberExt, Layer};

#[derive(Clone, Debug, Default)]
struct Capture(Arc<Mutex<Vec<SpanData>>>);
impl SpanExporter for Capture {
    async fn export(&self, spans: Vec<SpanData>) -> OTelSdkResult {
        self.0.lock().unwrap().extend(spans);
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn request_spans_preserve_responses_without_exporting_payloads_or_dynamic_paths() {
    let capture = Capture::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(capture.clone())
        .build();
    let layer = tracing_opentelemetry::layer()
        .with_tracer(provider.tracer("test"))
        .with_location(false)
        .with_threads(false)
        .with_filter(tracing_subscriber::filter::filter_fn(|m| {
            m.target() == "ccr_telemetry"
        }));
    let subscriber = tracing_subscriber::registry().with(layer);
    async {
        let app = ccr_rust::telemetry::instrument(
            Router::new()
                .route("/private/:name", post(|body: String| async move { body }))
                .route(
                    "/failure",
                    get(|| async { (StatusCode::TOO_MANY_REQUESTS, "SECRET_SENTINEL upstream") }),
                )
                .route(
                    "/stream",
                    get(|| async {
                        Body::from_stream(futures::stream::iter([
                            Ok::<_, std::io::Error>("first"),
                            Ok("second"),
                        ]))
                    }),
                ),
            true,
        );
        let request = Request::post("/private/SECRET_SENTINEL?key=SECRET_SENTINEL")
            .header("authorization", "Bearer SECRET_SENTINEL")
            .body(Body::from("SECRET_SENTINEL body"))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.unwrap(),
            "SECRET_SENTINEL body"
        );
        let response = app
            .clone()
            .oneshot(Request::get("/failure").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        to_bytes(response.into_body(), 1024).await.unwrap();
        let response = app
            .clone()
            .oneshot(Request::get("/stream").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.unwrap(),
            "firstsecond"
        );
        // Cancellation must release the span and preserve incomplete-stream state.
        drop(
            app.oneshot(Request::get("/stream").body(Body::empty()).unwrap())
                .await
                .unwrap(),
        );
    }
    .with_subscriber(subscriber)
    .await;
    provider.force_flush().unwrap();
    let spans = capture.0.lock().unwrap();
    assert_eq!(spans.len(), 4);
    let rendered = format!("{spans:?}");
    assert!(!rendered.contains("SECRET_SENTINEL"));
    assert!(!rendered.contains("code.file"));
    assert!(spans.iter().any(|s| s.status == Status::error("")));
    let completed: Vec<_> = spans
        .iter()
        .flat_map(|s| &s.attributes)
        .filter(|a| a.key.as_str() == "stream.completed")
        .map(|a| a.value.to_string())
        .collect();
    assert!(completed.iter().any(|v| v == "true"));
    assert!(completed.iter().any(|v| v == "false"));
    assert!(rendered.contains("/private/:name"));
}

#[tokio::test]
async fn disabled_telemetry_preserves_routing_without_an_exporter() {
    let app = ccr_rust::telemetry::instrument(
        Router::new().route("/health", get(|| async { "ok" })),
        false,
    );
    let response = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(to_bytes(response.into_body(), 1024).await.unwrap(), "ok");
}
