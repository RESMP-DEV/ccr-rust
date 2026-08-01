// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end coverage for Responses API upstream providers such as Meta Muse.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::Router;
use serde_json::json;
use tower::ServiceExt;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn build_app(config: ccr_rust::config::Config) -> Router {
    let state = ccr_rust::router::AppState {
        config,
        ewma_tracker: std::sync::Arc::new(ccr_rust::routing::EwmaTracker::new()),
        gp_router: None,
        transformer_registry: std::sync::Arc::new(ccr_rust::transformer::TransformerRegistry::new()),
        active_streams: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        max_streams: 0,
        ratelimit_tracker: std::sync::Arc::new(ccr_rust::ratelimit::RateLimitTracker::new()),
        shutdown_timeout: 30,
        debug_capture: None,
    };

    Router::new()
        .route("/v1/messages", post(ccr_rust::router::handle_messages))
        .with_state(state)
}

fn localhost_bind_available() -> bool {
    std::net::TcpListener::bind("127.0.0.1:0").is_ok()
}

#[tokio::test]
async fn meta_muse_uses_responses_endpoint_and_returns_anthropic_json() {
    if !localhost_bind_available() {
        eprintln!("Skipping test: localhost bind unavailable");
        return;
    }
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(header("authorization", "Bearer meta-test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "resp_meta_1",
            "object": "response",
            "created_at": 42,
            "status": "completed",
            "model": "muse-spark-1.1",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Meta Muse is routed."}]
            }],
            "usage": {"input_tokens": 9, "output_tokens": 5, "total_tokens": 14}
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let config_json = json!({
        "Providers": [{
            "name": "meta-muse",
            "api_base_url": upstream.uri(),
            "api_key": "meta-test-key",
            "models": ["muse-spark-1.1"],
            "protocol": "responses",
            "tier_name": "ccr-meta-muse"
        }],
        "Router": {
            "default": "meta-muse,muse-spark-1.1",
            "tiers": ["meta-muse,muse-spark-1.1"],
            "forceNonStreaming": true
        },
        "API_TIMEOUT_MS": 5000
    });
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, serde_json::to_vec(&config_json).unwrap()).unwrap();
    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "model": "auto",
                        "max_tokens": 64,
                        "messages": [{"role": "user", "content": "Route this through Muse"}],
                        "stream": false
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("x-ccr-tier").unwrap(),
        "ccr-meta-muse"
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["content"][0]["text"], "Meta Muse is routed.");
    assert_eq!(payload["usage"]["input_tokens"], 9);
    assert_eq!(payload["usage"]["output_tokens"], 5);

    let requests = upstream.received_requests().await.unwrap();
    let upstream_body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(upstream_body["model"], "muse-spark-1.1");
    assert_eq!(upstream_body["stream"], false);
    assert_eq!(upstream_body["max_output_tokens"], 64);
    assert_eq!(upstream_body["input"][0]["role"], "user");
    assert_eq!(
        upstream_body["input"][0]["content"][0],
        json!({"type": "input_text", "text": "Route this through Muse"})
    );
    assert!(upstream_body.get("messages").is_none());
}

#[tokio::test]
async fn meta_muse_wraps_completed_response_for_streaming_clients() {
    if !localhost_bind_available() {
        eprintln!("Skipping test: localhost bind unavailable");
        return;
    }
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "resp_meta_stream",
            "object": "response",
            "created_at": 43,
            "status": "completed",
            "model": "muse-spark-1.1",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "wrapped for stream"}]
            }],
            "usage": {"input_tokens": 7, "output_tokens": 3, "total_tokens": 10}
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let config_json = json!({
        "Providers": [{
            "name": "meta-muse",
            "api_base_url": upstream.uri(),
            "api_key": "meta-test-key",
            "models": ["muse-spark-1.1"],
            "protocol": "responses",
            "tier_name": "ccr-meta-muse"
        }],
        "Router": {
            "default": "meta-muse,muse-spark-1.1",
            "tiers": ["meta-muse,muse-spark-1.1"],
            "forceNonStreaming": false
        },
        "API_TIMEOUT_MS": 5000
    });
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, serde_json::to_vec(&config_json).unwrap()).unwrap();
    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();

    let response = build_app(config)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "model": "auto",
                        "max_tokens": 32,
                        "messages": [{"role": "user", "content": "Stream this"}],
                        "stream": true
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("event: message_start"));
    assert!(text.contains("wrapped for stream"));
    assert!(text.contains("event: message_stop"));

    let requests = upstream.received_requests().await.unwrap();
    let upstream_body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(upstream_body["stream"], false);
}
