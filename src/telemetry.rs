//! Optional, bounded export of deliberately minimal request spans to a local Collector.
use axum::{
    body::{Body, HttpBody},
    extract::MatchedPath,
    http::Request,
    middleware::Next,
    response::Response,
    Router,
};
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::{
    trace::{BatchConfigBuilder, BatchSpanProcessor, SdkTracerProvider},
    Resource,
};
use std::{
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tracing::{field::Empty, Instrument, Span};

mod runtime;
pub use runtime::{finish_command, initialize_provider};

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
    provider_for_endpoint(&endpoint)
}

/// Build the production exporter for an explicit loopback endpoint.
pub fn provider_for_endpoint(endpoint: &str) -> Option<SdkTracerProvider> {
    if !valid_endpoint(endpoint) {
        eprintln!("CCR telemetry disabled: endpoint must be a loopback HTTP /v1/traces URL");
        return None;
    }
    let client = match reqwest_otel::blocking::Client::builder()
        .redirect(reqwest_otel::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(client) => client,
        Err(_) => {
            eprintln!("CCR telemetry disabled: HTTP client initialization failed");
            return None;
        }
    };
    let exporter = match opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_http_client(client)
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
    Some(provider)
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
    app.layer(axum::middleware::from_fn(observe))
}

async fn observe(request: Request<Body>, next: Next) -> Response {
    let span = request_span(&request);
    let response = next.run(request).instrument(span.clone()).await;
    span.record("http.response.status_code", response.status().as_u16());
    if response.status().is_client_error() || response.status().is_server_error() {
        span.record("otel.status_code", "ERROR");
    }
    if response.body().is_end_stream() {
        span.record("stream.completed", true);
    }
    let (parts, inner) = response.into_parts();
    Response::from_parts(
        parts,
        Body::new(ObservedBody {
            inner,
            span,
            failed: false,
        }),
    )
}

// Forward every frame, error and size hint. Hyper can finish at the final frame
// without polling None, so check is_end_stream after polling as well.
struct ObservedBody {
    inner: Body,
    span: Span,
    failed: bool,
}

impl HttpBody for ObservedBody {
    type Data = bytes::Bytes;
    type Error = axum::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_frame(cx);
        match &result {
            Poll::Ready(Some(Err(_))) => {
                this.failed = true;
                this.span.record("otel.status_code", "ERROR");
            }
            Poll::Ready(None | Some(Ok(_))) if !this.failed && this.inner.is_end_stream() => {
                this.span.record("stream.completed", true);
            }
            Poll::Ready(None) if !this.failed => {
                this.span.record("stream.completed", true);
            }
            _ => {}
        }
        result
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
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
