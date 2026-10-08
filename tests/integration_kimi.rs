// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration tests for Kimi (Moonshot) backend routing.
//!
//! Tests end-to-end flow for Kimi K2.5 via ccr-rust's Anthropic protocol
//! handler, including:
//! - Direct routing with kimi,kimi-k2.5 model specification
//! - KimiTransformer think-token extraction
//! - Rate limit (429) passthrough
//! - Non-streaming JSON response handling

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::Router;
use serde_json::json;
use std::io::Write;
use tower::ServiceExt;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// Test Helpers
// ---------------------------------------------------------------------------

/// Write test config to temp file and return the path.
fn write_config_file(config_json: &serde_json::Value) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    serde_json::to_writer_pretty(&mut f, config_json).unwrap();
    f.flush().unwrap();
    f
}

/// Build test config with Kimi provider pointing at mock server.
fn make_kimi_config(mock_url: &str) -> serde_json::Value {
    json!({
        "Providers": [
            {
                "name": "kimi",
                "api_base_url": format!("{}/v1", mock_url),
                "api_key": "test-kimi-key",
                "models": ["kimi-k2.5", "kimi-k2-thinking"],
                "protocol": "anthropic",
                "tier_name": "ccr-kimi",
                "transformer": {
                    "use": ["kimi"]
                }
            }
        ],
        "Router": {
            "default": "kimi,kimi-k2.5"
        },
        "API_TIMEOUT_MS": 5000
    })
}

/// Build the Axum app with test state.
fn build_app(config: ccr_rust::config::Config) -> Router {
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
        admission_tracker: std::sync::Arc::new(ccr_rust::admission::AdmissionTracker::new()),
        retry_budget: std::sync::Arc::new(ccr_rust::retry_budget::RetryBudget::new()),
        sticky_sessions: std::sync::Arc::new(ccr_rust::stickiness::StickySessionTracker::new(
            std::time::Duration::from_secs(3600),
        )),
        shutdown_timeout: 30,
        debug_capture: None,
    };
    Router::new()
        .route("/v1/messages", post(ccr_rust::router::handle_messages))
        .with_state(state)
}

/// Helper to create Anthropic-style request for Kimi.
fn kimi_request_body(model: &str) -> serde_json::Value {
    json!({
        "model": model,
        "messages": [{"role": "user", "content": "Say hello in one word."}],
        "max_tokens": 100
    })
}

/// Skip integration tests that require opening localhost sockets.
fn skip_if_localhost_bind_unavailable(test_name: &str) -> bool {
    if std::net::TcpListener::bind("127.0.0.1:0").is_ok() {
        return false;
    }
    eprintln!("Skipping {test_name}: cannot bind localhost sockets");
    true
}

// ---------------------------------------------------------------------------
// Kimi Routing Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_kimi_direct_routing_with_provider_model() {
    if skip_if_localhost_bind_unavailable("test_kimi_direct_routing") {
        return;
    }

    let mock_server = MockServer::start().await;

    // Mock Kimi API response (Anthropic format)
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-kimi-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_kimi_123",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Hello"}],
            "model": "kimi-for-coding",
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 15,
                "output_tokens": 1
            }
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let config_json = make_kimi_config(&mock_server.uri());
    let config_file = write_config_file(&config_json);
    let config = ccr_rust::config::Config::from_file(config_file.path().to_str().unwrap()).unwrap();
    let app = build_app(config);

    // Request with kimi,kimi-k2.5 (direct routing)
    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("Content-Type", "application/json")
        .header("x-api-key", "client-key")
        .header("anthropic-version", "2023-06-01")
        .body(Body::from(
            serde_json::to_string(&kimi_request_body("kimi,kimi-k2.5")).unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), 10_000)
        .await
        .unwrap();
    let resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(resp["content"][0]["text"], "Hello");
}

#[tokio::test]
async fn test_kimi_thinking_tokens_extracted() {
    if skip_if_localhost_bind_unavailable("test_kimi_thinking_tokens") {
        return;
    }

    let mock_server = MockServer::start().await;

    // Mock Kimi response with Unicode think tokens
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_kimi_think",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "◁think▷User wants a greeting. Simple.◁/think▷Hello!"}],
            "model": "kimi-for-coding",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 15, "output_tokens": 10}
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let config_json = make_kimi_config(&mock_server.uri());
    let config_file = write_config_file(&config_json);
    let config = ccr_rust::config::Config::from_file(config_file.path().to_str().unwrap()).unwrap();
    let app = build_app(config);

    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("Content-Type", "application/json")
        .header("x-api-key", "client-key")
        .header("anthropic-version", "2023-06-01")
        .body(Body::from(
            serde_json::to_string(&kimi_request_body("kimi,kimi-k2.5")).unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), 10_000)
        .await
        .unwrap();
    let resp: serde_json::Value = serde_json::from_slice(&body).unwrap();

    // Think tokens should be stripped from content
    let text = resp["content"][0]["text"].as_str().unwrap();
    assert!(!text.contains("◁think▷"));
    assert_eq!(text, "Hello!");
}

#[tokio::test]
async fn test_kimi_429_rate_limit_passthrough() {
    if skip_if_localhost_bind_unavailable("test_kimi_429") {
        return;
    }

    let mock_server = MockServer::start().await;

    // Mock 429 rate limit response
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(429)
                .set_body_json(json!({
                    "error": {
                        "type": "rate_limit_error",
                        "message": "Too many requests"
                    }
                }))
                .append_header("retry-after", "30"),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let config_json = make_kimi_config(&mock_server.uri());
    let config_file = write_config_file(&config_json);
    let config = ccr_rust::config::Config::from_file(config_file.path().to_str().unwrap()).unwrap();
    let app = build_app(config);

    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("Content-Type", "application/json")
        .header("x-api-key", "client-key")
        .header("anthropic-version", "2023-06-01")
        .body(Body::from(
            serde_json::to_string(&kimi_request_body("kimi,kimi-k2.5")).unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // 429 should be passed through (not converted to 500)
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn test_kimi_anthropic_version_header_sent() {
    if skip_if_localhost_bind_unavailable("test_kimi_headers") {
        return;
    }

    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-kimi-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_kimi_hdr",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "OK"}],
            "model": "kimi-for-coding",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let config_json = make_kimi_config(&mock_server.uri());
    let config_file = write_config_file(&config_json);
    let config = ccr_rust::config::Config::from_file(config_file.path().to_str().unwrap()).unwrap();
    let app = build_app(config);

    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("Content-Type", "application/json")
        .header("x-api-key", "client-key")
        .header("anthropic-version", "2023-06-01")
        .body(Body::from(
            serde_json::to_string(&kimi_request_body("kimi,kimi-k2.5")).unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
