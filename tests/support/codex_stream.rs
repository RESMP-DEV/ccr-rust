use axum::body::Body;
use axum::http::Request;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

pub(crate) fn skip_if_localhost_bind_unavailable() -> bool {
    if std::net::TcpListener::bind("127.0.0.1:0").is_ok() {
        return false;
    }

    eprintln!("Skipping test: cannot bind localhost sockets in this environment");
    true
}

pub(crate) fn make_test_config(protocol: &str, base_url: &str) -> String {
    let config = json!({
        "Providers": [
            {
                "name": "mock",
                "api_base_url": base_url,
                "api_key": "test-key",
                "models": ["test-model"],
                "protocol": protocol
            }
        ],
        "Router": {
            "default": "mock,test-model"
        },
        "API_TIMEOUT_MS": 5000
    });

    serde_json::to_string_pretty(&config).unwrap()
}

pub(crate) fn build_app(config: ccr_rust::config::Config) -> Router {
    let ewma_tracker = std::sync::Arc::new(ccr_rust::routing::EwmaTracker::new());
    let transformer_registry =
        std::sync::Arc::new(ccr_rust::transformer::TransformerRegistry::new());
    let active_streams = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ratelimit_tracker = std::sync::Arc::new(ccr_rust::ratelimit::RateLimitTracker::new());
    let state = ccr_rust::router::AppState {
        config,
        ewma_tracker,
        gp_router: None,
        transformer_registry,
        active_streams,
        max_streams: 0,
        ratelimit_tracker,
        shutdown_timeout: 30,
        debug_capture: None,
    };

    Router::new()
        .route("/v1/messages", post(ccr_rust::router::handle_messages))
        .route(
            "/v1/chat/completions",
            post(ccr_rust::router::handle_chat_completions),
        )
        .route("/v1/models", get(ccr_rust::router::list_models))
        .with_state(state)
}

pub(crate) fn codex_stream_request_body() -> Value {
    json!({
        "model": "mock,test-model",
        "messages": [
            {"role": "user", "content": "Stream please"}
        ],
        "max_tokens": 100,
        "stream": true
    })
}

pub(crate) fn parse_sse_data_frames(payload: &str) -> Vec<String> {
    let normalized = payload.replace("\r\n", "\n");
    normalized
        .split("\n\n")
        .filter_map(|frame| {
            if frame.trim().is_empty() {
                return None;
            }

            let mut data_lines = Vec::new();
            for line in frame.lines() {
                if let Some(rest) = line.strip_prefix("data:") {
                    data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
                }
            }

            if data_lines.is_empty() {
                None
            } else {
                Some(data_lines.join("\n"))
            }
        })
        .collect()
}

pub(crate) async fn make_codex_stream_request(app: &Router) -> Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("user-agent", "codex-cli/1.0.0")
                .body(Body::from(
                    serde_json::to_vec(&codex_stream_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
}
