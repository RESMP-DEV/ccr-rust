//! Optional, bounded export of deliberately minimal request spans to a local Collector.
use axum::{body::Body, extract::MatchedPath, http::Request, Router};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    trace::{BatchConfigBuilder, BatchSpanProcessor, SdkTracerProvider},
    Resource,
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tower_http::{
    classify::{ClassifiedResponse, ClassifyEos, ClassifyResponse, SharedClassifier},
    trace::TraceLayer,
};
use tracing::{field::Empty, Span};

static ENABLED: AtomicBool = AtomicBool::new(false);

// The stock HTTP status classifier finishes at headers and suppresses on_eos.
// Wait for body completion without recording error strings or changing frames.
#[derive(Clone)]
struct CompletionClassifier;

impl ClassifyResponse for CompletionClassifier {
    type FailureClass = ();
    type ClassifyEos = Self;
    fn classify_response<B>(self, _: &axum::http::Response<B>) -> ClassifiedResponse<(), Self> {
        ClassifiedResponse::RequiresEos(self)
    }
    fn classify_error<E: std::fmt::Display + 'static>(self, _: &E) {}
}

impl ClassifyEos for CompletionClassifier {
    type FailureClass = ();
    fn classify_eos(self, _: Option<&axum::http::HeaderMap>) -> Result<(), ()> {
        Ok(())
    }
    fn classify_error<E: std::fmt::Display + 'static>(self, _: &E) {}
}

fn valid_endpoint(value: &str) -> bool {
    reqwest::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "http"
            && matches!(url.host_str(), Some("127.0.0.1" | "[::1]"))
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/v1/traces"
    })
}

/// Construct on a blocking thread: the SDK owns its bounded export worker.
pub fn provider_from_env() -> Option<SdkTracerProvider> {
    let endpoint = std::env::var("CCR_OTEL_ENDPOINT").ok()?;
    if !valid_endpoint(&endpoint) {
        eprintln!("CCR telemetry disabled: endpoint must be a loopback HTTP /v1/traces URL");
        return None;
    }
    let exporter = match opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .with_timeout(Duration::from_secs(2))
        .build()
    {
        Ok(exporter) => exporter,
        Err(_) => {
            eprintln!("CCR telemetry disabled: exporter initialization failed");
            return None;
        }
    };
    let batch = BatchSpanProcessor::builder(exporter)
        .with_batch_config(
            BatchConfigBuilder::default()
                .with_max_queue_size(1024)
                .with_max_export_batch_size(128)
                .with_scheduled_delay(Duration::from_secs(2))
                .build(),
        )
        .build();
    let provider = SdkTracerProvider::builder()
        .with_span_processor(batch)
        .with_resource(
            Resource::builder_empty()
                .with_service_name("ccr-rust")
                .build(),
        )
        .build();
    ENABLED.store(true, Ordering::Relaxed);
    Some(provider)
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

fn request_span(request: &Request<Body>) -> Span {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", MatchedPath::as_str);
    let method = match request.method().as_str() {
        "GET" => "GET",
        "POST" => "POST",
        "OPTIONS" => "OPTIONS",
        _ => "OTHER",
    };
    // Never record URI/query, headers, bodies, errors, provider responses, or paths.
    tracing::info_span!(target: "ccr_telemetry", "http.request",
        otel.kind = "server", otel.status_code = Empty,
        http.request.method = method, http.route = route,
        http.response.status_code = Empty, stream.completed = false)
}

pub fn instrument(app: Router, enabled: bool) -> Router {
    if !enabled {
        return app;
    }
    app.layer(
        TraceLayer::new(SharedClassifier::new(CompletionClassifier))
            .make_span_with(request_span as fn(&Request<Body>) -> Span)
            .on_request(())
            .on_body_chunk(())
            .on_response(
                |response: &axum::http::Response<Body>, _: Duration, span: &Span| {
                    span.record("http.response.status_code", response.status().as_u16());
                    if response.status().is_client_error() || response.status().is_server_error() {
                        span.record("otel.status_code", "ERROR");
                    }
                },
            )
            .on_eos(
                |_: Option<&axum::http::HeaderMap>, _: Duration, span: &Span| {
                    span.record("stream.completed", true);
                },
            )
            .on_failure(|_: (), _: Duration, span: &Span| {
                span.record("otel.status_code", "ERROR");
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collector_endpoint_cannot_export_to_remote_or_include_credentials() {
        assert!(valid_endpoint("http://127.0.0.1:4318/v1/traces"));
        assert!(valid_endpoint("http://[::1]:4318/v1/traces"));
        for value in [
            "https://example.com/v1/traces",
            "http://127.0.0.1.evil/v1/traces",
            "http://user:secret@127.0.0.1/v1/traces",
            "http://127.0.0.1/v1/traces?secret=x",
            "http://127.0.0.1/v1/logs",
        ] {
            assert!(!valid_endpoint(value));
        }
    }
}
