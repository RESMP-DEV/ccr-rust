#![cfg(feature = "telemetry")]
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    routing::{get, post},
    Router,
};
use http_body_util::BodyExt;
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
                .route("/fixed", get(|| async { "fixed" }))
                .route(
                    "/body-error",
                    get(|| async {
                        Body::from_stream(futures::stream::iter([Err::<bytes::Bytes, _>(
                            std::io::Error::other("SECRET_SENTINEL body error"),
                        )]))
                    }),
                )
                .route(
                    "/trailers",
                    get(|| async {
                        let mut trailers = axum::http::HeaderMap::new();
                        trailers.insert("x-test", "preserved".parse().unwrap());
                        Body::new(http_body_util::StreamBody::new(futures::stream::iter([
                            Ok::<_, std::io::Error>(http_body::Frame::data(
                                bytes::Bytes::from_static(b"payload"),
                            )),
                            Ok(http_body::Frame::trailers(trailers)),
                        ])))
                    }),
                )
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
        let response = app
            .clone()
            .oneshot(Request::get("/fixed").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let mut body = response.into_body();
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            "fixed"
        );
        assert!(axum::body::HttpBody::is_end_stream(&body));
        drop(body); // Hyper need not poll None after this final data frame.
        let response = app
            .clone()
            .oneshot(Request::get("/body-error").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(to_bytes(response.into_body(), 1024).await.is_err());
        let response = app
            .clone()
            .oneshot(Request::get("/trailers").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let collected = response.into_body().collect().await.unwrap();
        assert_eq!(collected.trailers().unwrap()["x-test"], "preserved");
        assert_eq!(collected.to_bytes(), "payload");
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
    assert_eq!(spans.len(), 7);
    let span_for = |route: &str| {
        spans
            .iter()
            .find(|s| {
                s.attributes
                    .iter()
                    .any(|a| a.key.as_str() == "http.route" && a.value.to_string() == route)
            })
            .unwrap()
    };
    assert!(span_for("/fixed")
        .attributes
        .iter()
        .any(|a| a.key.as_str() == "stream.completed" && a.value.to_string() == "true"));
    assert!(matches!(
        span_for("/body-error").status,
        Status::Error { .. }
    ));
    let rendered = format!("{spans:?}");
    assert!(!rendered.contains("SECRET_SENTINEL"));
    assert!(!rendered.contains("code.file"));
    assert!(spans
        .iter()
        .any(|s| matches!(s.status, Status::Error { .. })));
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
