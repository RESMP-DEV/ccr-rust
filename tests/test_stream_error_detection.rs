// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for detecting provider errors embedded in HTTP 200 streaming responses.
//!
//! Some providers (e.g. Z.AI) return quota/overload errors as raw JSON inside a
//! `text/event-stream` 200 response instead of using HTTP 429/5xx status codes.
//! CCR-Rust must detect these and cascade to the next tier.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::Router;
use serde_json::json;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
        max_streams: 512,
        ratelimit_tracker,
        shutdown_timeout: 30,
        debug_capture: None,
    };
    Router::new()
        .route("/v1/messages", post(ccr_rust::router::handle_messages))
        .with_state(state)
}

fn skip_if_localhost_bind_unavailable(test_name: &str) -> bool {
    if std::net::TcpListener::bind("127.0.0.1:0").is_ok() {
        return false;
    }
    eprintln!("Skipping {test_name}: cannot bind localhost sockets");
    true
}

/// Z.AI returns a quota exhaustion error as an SSE frame inside an HTTP 200
/// `text/event-stream` body. The `stream: true` flag exercises
/// `check_stream_for_embedded_error` (the first-chunk peek path).
#[tokio::test]
async fn stream_error_in_200_cascades_to_next_tier() {
    if skip_if_localhost_bind_unavailable("stream_error_in_200_cascades_to_next_tier") {
        return;
    }

    let tier0_server = MockServer::start().await;
    let tier1_server = MockServer::start().await;

    // tier-0 (Z.AI): returns 200 with error JSON wrapped in an SSE data frame
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    "data: {\"error\":{\"code\":\"1310\",\"message\":\"Weekly/Monthly Limit Exhausted. Your limit will reset at 2026-04-15 10:20:22\"}}\n\n",
                ),
        )
        .mount(&tier0_server)
        .await;

    // tier-1 (Wafer): returns a valid OpenAI response
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-ok",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "qwen",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hello from tier-1!"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        })))
        .expect(1)
        .mount(&tier1_server)
        .await;

    let config = json!({
        "Providers": [
            {
                "name": "zai",
                "api_base_url": tier0_server.uri(),
                "api_key": "key0",
                "models": ["glm-5.1"]
            },
            {
                "name": "wafer",
                "api_base_url": tier1_server.uri(),
                "api_key": "key1",
                "models": ["qwen"]
            }
        ],
        "Router": {
            "default": "zai,glm-5.1",
            "think": "wafer,qwen",
            "tierRetries": {
                "tier-0": { "max_retries": 0 },
                "tier-1": { "max_retries": 0 }
            }
        },
        "API_TIMEOUT_MS": 5000
    });

    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let cfg = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(cfg);

    let body = json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}],
        "max_tokens": 100,
        "stream": true
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "Request should succeed via tier-1 after tier-0 returns error in 200 stream"
    );
}

/// Z.AI overload error (code 1305) in a streaming 200 body should also
/// cascade. Uses raw JSON (no SSE `data:` prefix) to cover that variant.
#[tokio::test]
async fn overload_error_in_200_cascades() {
    if skip_if_localhost_bind_unavailable("overload_error_in_200_cascades") {
        return;
    }

    let tier0_server = MockServer::start().await;
    let tier1_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    r#"{"error":{"code":"1305","message":"Service temporarily overloaded, please try again later"}}"#,
                ),
        )
        .mount(&tier0_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-ok",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "qwen",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "recovered"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        })))
        .expect(1)
        .mount(&tier1_server)
        .await;

    let config = json!({
        "Providers": [
            { "name": "zai", "api_base_url": tier0_server.uri(), "api_key": "k", "models": ["m0"] },
            { "name": "wafer", "api_base_url": tier1_server.uri(), "api_key": "k", "models": ["m1"] }
        ],
        "Router": {
            "default": "zai,m0",
            "think": "wafer,m1",
            "tierRetries": { "tier-0": { "max_retries": 0 }, "tier-1": { "max_retries": 0 } }
        },
        "API_TIMEOUT_MS": 5000
    });

    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let cfg = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(cfg);

    let body = json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}],
        "max_tokens": 100,
        "stream": true
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

/// A valid response from tier-0 should NOT be treated as an error.
#[tokio::test]
async fn valid_200_response_not_treated_as_error() {
    if skip_if_localhost_bind_unavailable("valid_200_response_not_treated_as_error") {
        return;
    }

    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-ok",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "test",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hello!"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let config = json!({
        "Providers": [
            { "name": "mock", "api_base_url": mock_server.uri(), "api_key": "k", "models": ["m"] }
        ],
        "Router": { "default": "mock,m" },
        "API_TIMEOUT_MS": 5000
    });

    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let cfg = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(cfg);

    let body = json!({
        "model": "mock,m",
        "messages": [{"role": "user", "content": "hello"}],
        "max_tokens": 100
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}
