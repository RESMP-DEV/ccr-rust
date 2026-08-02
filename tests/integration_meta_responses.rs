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
            "metadata": {"campaign": "native-envelope"},
            "previous_response_id": "resp_previous",
            "instructions": "Keep the native response envelope.",
            "tools": [{"type": "web_search", "search_context_size": "low"}],
            "text": {"format": {"type": "text"}},
            "__ccr_responses_response": {"id": "provider-injected"},
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
    assert!(payload.get("metadata").is_none());
    assert!(payload.get("__ccr_responses_response").is_none());

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
async fn native_background_response_preserves_queued_empty_envelope() {
    if !localhost_bind_available() {
        eprintln!("Skipping test: localhost bind unavailable");
        return;
    }
    let upstream = MockServer::start().await;
    let queued_response = json!({
        "id": "resp_background",
        "object": "response",
        "created_at": 45,
        "status": "queued",
        "background": true,
        "model": "muse-spark-1.1",
        "metadata": {"campaign": "background"},
        "output": []
    });
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(queued_response.clone()))
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
                        "input": "run in the background",
                        "background": true,
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
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload, queued_response);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "model": "auto",
                        "input": "stream the background receipt",
                        "background": true,
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
    let events = sse_json_events(std::str::from_utf8(&body).unwrap()).unwrap();
    let queued = events
        .iter()
        .find(|event| event["type"] == "response.queued")
        .expect("queued stream event should preserve the pollable envelope");
    assert_eq!(queued["response"], queued_response);
    assert!(!events
        .iter()
        .any(|event| event["type"] == "response.incomplete"));
}

#[tokio::test]
async fn native_failed_response_preserves_tracking_envelope() {
    if !localhost_bind_available() {
        eprintln!("Skipping test: localhost bind unavailable");
        return;
    }
    let upstream = MockServer::start().await;
    let failed_response = json!({
        "id": "resp_failed",
        "object": "response",
        "created_at": 46,
        "status": "failed",
        "model": "muse-spark-1.1",
        "error": {"code": "upstream_error", "message": "generation failed"},
        "output": []
    });
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(failed_response.clone()))
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
            "tiers": ["meta-muse,muse-spark-1.1"]
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
                .uri("/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "model": "auto",
                        "input": "fail with a tracking receipt",
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
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload, failed_response);
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
            "metadata": {"campaign": "native-envelope"},
            "previous_response_id": "resp_previous",
            "instructions": "Keep the native response envelope.",
            "tools": [{"type": "web_search", "search_context_size": "low"}],
            "text": {"format": {"type": "text"}},
            "output": [{
                "id": "rs_native",
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "Reasoning survives adapters"}]
            }, {
                "type": "web_search_call",
                "id": "ws_1",
                "status": "completed",
                "action": {"type": "search", "query": "adapter citations"}
            }, {
                "id": "msg_native",
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": "streamed through adapters",
                    "annotations": [{
                        "type": "url_citation",
                        "start_index": 0,
                        "end_index": 8,
                        "title": "Adapter source",
                        "url": "https://example.test/source"
                    }]
                }]
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
                "stream": true,
                "response_format": {
                    "type": "json_schema",
                    "json_schema": {
                        "name": "chat_answer",
                        "schema": {
                            "type": "object",
                            "properties": {"answer": {"type": "string"}},
                            "required": ["answer"],
                            "additionalProperties": false
                        },
                        "strict": true
                    }
                },
                "__ccr_responses_request": {
                    "model": "spoofed",
                    "input": "do not forward this"
                },
                "__ccr_responses_response": {"id": "spoofed-response"}
            }),
        ),
        (
            "/v1/responses",
            json!({
                "model": "auto",
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": "stream responses"
                    }, {
                        "type": "input_file",
                        "file_id": "file_123"
                    }]
                }],
                "stream": true,
                "tools": [{"type": "web_search", "search_context_size": "low"}],
                "tool_choice": {"type": "web_search"},
                "text": {
                    "format": {
                        "type": "json_schema",
                        "name": "answer",
                        "schema": {
                            "type": "object",
                            "properties": {"answer": {"type": "string"}},
                            "required": ["answer"],
                            "additionalProperties": false
                        },
                        "strict": true
                    }
                }
            }),
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
            assert_eq!(
                output.len(),
                3,
                "reasoning, native tool, and message items are required"
            );
            let added = events
                .iter()
                .filter(|event| event["type"] == "response.output_item.added")
                .collect::<Vec<_>>();
            let done = events
                .iter()
                .filter(|event| event["type"] == "response.output_item.done")
                .collect::<Vec<_>>();
            assert_eq!(added.len(), output.len());
            assert_eq!(done.len(), output.len());
            for (output_index, item) in output.iter().enumerate() {
                assert_eq!(added[output_index]["output_index"], output_index);
                assert_eq!(done[output_index]["output_index"], output_index);
                assert_eq!(done[output_index]["item"], *item);
                match item["type"].as_str() {
                    Some("reasoning") => {
                        assert_eq!(added[output_index]["item"]["summary"], json!([]));
                    }
                    Some("message") => {
                        assert_eq!(added[output_index]["item"]["content"], json!([]));
                    }
                    Some("function_call") => {
                        assert_eq!(added[output_index]["item"]["arguments"], "");
                    }
                    _ => assert_eq!(added[output_index]["item"], *item),
                }
            }
            assert!(!events
                .iter()
                .any(|event| event["type"] == "response.reasoning_text.delta"));
            let output_text_delta = events
                .iter()
                .find(|event| event["type"] == "response.output_text.delta")
                .expect("output text delta should be replayed with item identity");
            assert_eq!(output_text_delta["output_index"], 2);
            assert_eq!(output_text_delta["item_id"], "msg_native");
            assert_eq!(output_text_delta["content_index"], 0);
            assert_eq!(output[0]["type"], "reasoning");
            assert_eq!(
                output[0]["summary"][0],
                json!({
                    "type": "summary_text",
                    "text": "Reasoning survives adapters"
                })
            );
            assert_eq!(output[1]["type"], "web_search_call");
            assert_eq!(output[1]["id"], "ws_1");
            assert_eq!(output[1]["action"]["query"], "adapter citations");
            assert_eq!(output[2]["type"], "message");
            assert_eq!(
                output[2]["content"],
                json!([{
                    "type": "output_text",
                    "text": "streamed through adapters",
                    "annotations": [{
                        "type": "url_citation",
                        "start_index": 0,
                        "end_index": 8,
                        "title": "Adapter source",
                        "url": "https://example.test/source"
                    }]
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
            assert_eq!(
                completed["response"]["metadata"],
                json!({"campaign": "native-envelope"})
            );
            assert_eq!(
                completed["response"]["previous_response_id"],
                "resp_previous"
            );
            assert_eq!(
                completed["response"]["instructions"],
                "Keep the native response envelope."
            );
            assert_eq!(
                completed["response"]["tools"],
                json!([{"type": "web_search", "search_context_size": "low"}])
            );
            assert_eq!(
                completed["response"]["text"],
                json!({"format": {"type": "text"}})
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
                        "stream": false,
                        "tool_choice": null
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
    assert_eq!(response_json["output"][1]["type"], "web_search_call");
    assert_eq!(response_json["output"][1]["id"], "ws_1");
    assert_eq!(response_json["output"][2]["type"], "message");
    assert_eq!(
        response_json["output"][2]["content"],
        json!([{
            "type": "output_text",
            "text": "streamed through adapters",
            "annotations": [{
                "type": "url_citation",
                "start_index": 0,
                "end_index": 8,
                "title": "Adapter source",
                "url": "https://example.test/source"
            }]
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
    assert_eq!(
        response_json["metadata"],
        json!({"campaign": "native-envelope"})
    );
    assert_eq!(response_json["previous_response_id"], "resp_previous");
    assert_eq!(
        response_json["instructions"],
        "Keep the native response envelope."
    );
    assert_eq!(
        response_json["tools"],
        json!([{"type": "web_search", "search_context_size": "low"}])
    );
    assert_eq!(response_json["text"], json!({"format": {"type": "text"}}));

    let requests = upstream.received_requests().await.unwrap();
    assert!(requests.iter().all(|request| {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        body["stream"] == false
            && body.get("__ccr_responses_request").is_none()
            && body.get("__ccr_responses_output").is_none()
            && body.get("__ccr_responses_response").is_none()
            && body["model"] != "spoofed"
            && !body
                .get("tool_choice")
                .is_some_and(|choice| choice.is_null())
    }));
    assert!(requests.iter().any(|request| {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        body["tools"] == json!([{"type": "web_search", "search_context_size": "low"}])
            && body["tool_choice"] == json!({"type": "web_search"})
            && body["text"]["format"]["type"] == "json_schema"
            && body["text"]["format"]["name"] == "answer"
            && body["input"].as_array().is_some_and(|items| {
                items.iter().any(|item| {
                    item["role"] == "user"
                        && item["content"].as_array().is_some_and(|content| {
                            content.iter().any(|part| {
                                part["type"] == "input_text" && part["text"] == "stream responses"
                            }) && content.iter().any(|part| {
                                part["type"] == "input_file" && part["file_id"] == "file_123"
                            })
                        })
                })
            })
    }));
    assert!(requests.iter().any(|request| {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        body["text"]["format"]
            == json!({
                "type": "json_schema",
                "name": "chat_answer",
                "schema": {
                    "type": "object",
                    "properties": {"answer": {"type": "string"}},
                    "required": ["answer"],
                    "additionalProperties": false
                },
                "strict": true
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
                            "previous_response_id": "resp_previous",
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

    for stream in [false, true] {
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
                            "max_tokens": 64,
                            "messages": [{"role": "user", "content": "refuse this"}],
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
            assert!(text.contains("I cannot help with that."));
            assert!(text.contains("text_delta"));
        } else {
            let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                payload["content"][0],
                json!({"type": "text", "text": "I cannot help with that."})
            );
            assert!(payload.get("refusal").is_none());
        }
    }

    let requests = upstream.received_requests().await.unwrap();
    let continued_requests = requests
        .iter()
        .filter(|request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            body.get("previous_response_id").is_some()
        })
        .collect::<Vec<_>>();
    assert_eq!(continued_requests.len(), 2);
    assert!(continued_requests.iter().all(|request| {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        body["reasoning"] == json!({"effort": "high", "summary": "detailed"})
            && body["previous_response_id"] == "resp_previous"
    }));
}
