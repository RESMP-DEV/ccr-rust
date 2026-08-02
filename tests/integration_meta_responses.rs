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
        .route(
            "/v1/chat/completions",
            post(ccr_rust::router::handle_chat_completions),
        )
        .route("/v1/responses", post(ccr_rust::router::handle_responses))
        .with_state(state)
}

fn localhost_bind_available() -> bool {
    std::net::TcpListener::bind("127.0.0.1:0").is_ok()
}

fn sse_json_events(payload: &str) -> Result<Vec<serde_json::Value>, serde_json::Error> {
    payload
        .split("\n\n")
        .filter_map(|frame| {
            let data = frame
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(|line| line.strip_prefix(' ').unwrap_or(line))
                .collect::<Vec<_>>()
                .join("\n");
            (!data.is_empty() && data != "[DONE]").then_some(data)
        })
        .map(|data| serde_json::from_str(&data))
        .collect()
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
            "error": null,
            "incomplete_details": null,
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

#[tokio::test]
async fn meta_muse_preserves_streaming_for_openai_frontends() {
    if !localhost_bind_available() {
        eprintln!("Skipping test: localhost bind unavailable");
        return;
    }
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "resp_meta_openai_stream",
            "object": "response",
            "created_at": 44,
            "status": "completed",
            "error": null,
            "incomplete_details": null,
            "model": "muse-spark-1.1",
            "output": [{
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "Reasoning survives adapters"}]
            }, {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "streamed through adapters"}]
            }],
            "usage": {
                "input_tokens": 8,
                "input_tokens_details": {"cached_tokens": 2},
                "output_tokens": 4,
                "output_tokens_details": {"reasoning_tokens": 3},
                "total_tokens": 12
            }
        })))
        .expect(4)
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
            "tiers": ["meta-muse,muse-spark-1.1"]
        },
        "API_TIMEOUT_MS": 5000
    });
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, serde_json::to_vec(&config_json).unwrap()).unwrap();
    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    for (uri, payload) in [
        (
            "/v1/chat/completions",
            json!({
                "model": "auto",
                "messages": [{"role": "user", "content": "stream chat"}],
                "stream": true
            }),
        ),
        (
            "/v1/responses",
            json!({"model": "auto", "input": "stream responses", "stream": true}),
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        assert_eq!(response.headers()["x-ccr-tier"], "ccr-meta-muse");
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("streamed through adapters"));
        assert!(text.contains("Reasoning survives adapters"));

        if uri == "/v1/responses" {
            let events = sse_json_events(&text).expect("Responses SSE data should be valid JSON");
            let completed = events
                .iter()
                .find(|event| event["type"] == "response.completed")
                .expect("Responses stream should contain response.completed");
            let output = completed["response"]["output"]
                .as_array()
                .expect("completed Responses output should be an array");
            assert_eq!(output.len(), 2, "reasoning and message items are required");
            assert_eq!(output[0]["type"], "reasoning");
            assert_eq!(
                output[0]["summary"][0],
                json!({
                    "type": "summary_text",
                    "text": "Reasoning survives adapters"
                })
            );
            assert_eq!(output[1]["type"], "message");
            assert_eq!(
                output[1]["content"],
                json!([{
                    "type": "output_text",
                    "text": "streamed through adapters"
                }])
            );
            assert_eq!(
                completed["response"]["usage"]["input_tokens_details"]["cached_tokens"],
                2
            );
            assert_eq!(
                completed["response"]["usage"]["output_tokens_details"]["reasoning_tokens"],
                3
            );
        }
    }

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .header("anthropic-version", "2023-06-01")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "model": "auto",
                        "messages": [{"role": "user", "content": "native stream"}],
                        "max_tokens": 256,
                        "stream": true
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let native_stream = String::from_utf8(body.to_vec()).unwrap();
    assert!(native_stream.contains("streamed through adapters"));
    assert!(!native_stream.contains("thinking_delta"));
    assert!(!native_stream.contains("Reasoning survives adapters"));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "model": "auto",
                        "input": "non-stream responses",
                        "stream": false
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(response_json["output"][0]["type"], "reasoning");
    assert_eq!(
        response_json["output"][0]["summary"][0]["text"],
        "Reasoning survives adapters"
    );
    assert_eq!(response_json["output"][1]["type"], "message");
    assert_eq!(
        response_json["output"][1]["content"],
        json!([{
            "type": "output_text",
            "text": "streamed through adapters"
        }])
    );
    assert_eq!(
        response_json["usage"]["input_tokens_details"]["cached_tokens"],
        2
    );
    assert_eq!(
        response_json["usage"]["output_tokens_details"]["reasoning_tokens"],
        3
    );

    let requests = upstream.received_requests().await.unwrap();
    assert!(requests.iter().all(|request| {
        serde_json::from_slice::<serde_json::Value>(&request.body).unwrap()["stream"] == false
    }));
    assert!(requests.iter().any(|request| {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        body["input"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item["role"] == "user"
                    && item["content"].as_array().is_some_and(|content| {
                        content.iter().any(|part| {
                            part["type"] == "input_text" && part["text"] == "stream responses"
                        })
                    })
            })
        })
    }));
}

#[tokio::test]
async fn meta_muse_preserves_reasoning_refusal_and_incomplete_status() {
    if !localhost_bind_available() {
        eprintln!("Skipping test: localhost bind unavailable");
        return;
    }
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "resp_meta_refusal",
            "object": "response",
            "created_at": 45,
            "status": "incomplete",
            "incomplete_details": {"reason": "content_filter"},
            "model": "muse-spark-1.1",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "refusal",
                    "refusal": "I cannot help with that."
                }]
            }],
            "usage": {"input_tokens": 5, "output_tokens": 2, "total_tokens": 7}
        })))
        .expect(2)
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
            "tiers": ["meta-muse,muse-spark-1.1"]
        },
        "API_TIMEOUT_MS": 5000
    });
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, serde_json::to_vec(&config_json).unwrap()).unwrap();
    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    for stream in [false, true] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "model": "auto",
                            "input": "refuse this",
                            "reasoning": {"effort": "high", "summary": "detailed"},
                            "stream": stream
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        if stream {
            let text = String::from_utf8(body.to_vec()).unwrap();
            let events = sse_json_events(&text).expect("Responses SSE data should be valid JSON");
            let incomplete = events
                .iter()
                .find(|event| event["type"] == "response.incomplete")
                .expect("Responses stream should preserve incomplete status");
            assert_eq!(
                incomplete["response"]["output"][0]["content"][0],
                json!({
                    "type": "refusal",
                    "refusal": "I cannot help with that."
                })
            );
            assert_eq!(
                incomplete["response"]["incomplete_details"],
                json!({"reason": "content_filter"})
            );
        } else {
            let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(payload["status"], "incomplete");
            assert_eq!(
                payload["incomplete_details"],
                json!({"reason": "content_filter"})
            );
            assert_eq!(
                payload["output"][0]["content"][0],
                json!({
                    "type": "refusal",
                    "refusal": "I cannot help with that."
                })
            );
        }
    }

    let requests = upstream.received_requests().await.unwrap();
    assert!(requests.iter().all(|request| {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        body["reasoning"] == json!({"effort": "high", "summary": "detailed"})
    }));
}
