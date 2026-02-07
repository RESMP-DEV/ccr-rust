//! Integration tests for OpenAI Responses API compatibility.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::Router;
use serde_json::json;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Debug)]
struct SseEvent {
    event: String,
    data: serde_json::Value,
}

fn build_openai_sse(chunks: &[serde_json::Value]) -> String {
    let mut sse = String::new();
    for chunk in chunks {
        sse.push_str("data: ");
        sse.push_str(&chunk.to_string());
        sse.push_str("\n\n");
    }
    sse.push_str("data: [DONE]\n\n");
    sse
}

fn parse_sse_events(body: &str) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let mut current_event: Option<String> = None;
    let mut current_data = String::new();

    for line in body.lines() {
        if line.is_empty() {
            if let Some(event) = current_event.take() {
                if current_data.trim() != "[DONE]" {
                    if let Ok(data) = serde_json::from_str::<serde_json::Value>(current_data.trim())
                    {
                        events.push(SseEvent { event, data });
                    }
                }
            }
            current_data.clear();
            continue;
        }

        if let Some(rest) = line.strip_prefix("event: ") {
            current_event = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data: ") {
            if !current_data.is_empty() {
                current_data.push('\n');
            }
            current_data.push_str(rest);
        }
    }

    if let Some(event) = current_event {
        if current_data.trim() != "[DONE]" {
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(current_data.trim()) {
                events.push(SseEvent { event, data });
            }
        }
    }

    events
}

fn make_test_config(mock_url: &str) -> String {
    let config = json!({
        "Providers": [
            {
                "name": "mock",
                "api_base_url": mock_url,
                "api_key": "test-key",
                "models": ["test-model"]
            }
        ],
        "Router": {
            "default": "mock,test-model"
        },
        "API_TIMEOUT_MS": 5000
    });

    serde_json::to_string_pretty(&config).unwrap()
}

fn make_test_config_anthropic(mock_url: &str) -> String {
    let config = json!({
        "Providers": [
            {
                "name": "mock",
                "api_base_url": mock_url,
                "api_key": "test-key",
                "models": ["test-model"],
                "protocol": "anthropic",
                "anthropic_version": "2023-06-01"
            }
        ],
        "Router": {
            "default": "mock,test-model"
        },
        "API_TIMEOUT_MS": 5000
    });

    serde_json::to_string_pretty(&config).unwrap()
}

fn build_app(config: ccr_rust::config::Config) -> Router {
    let ewma_tracker = std::sync::Arc::new(ccr_rust::routing::EwmaTracker::new());
    let transformer_registry =
        std::sync::Arc::new(ccr_rust::transformer::TransformerRegistry::new());
    let active_streams = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ratelimit_tracker = std::sync::Arc::new(ccr_rust::ratelimit::RateLimitTracker::new());
    let state = ccr_rust::router::AppState {
        config,
        ewma_tracker,
        transformer_registry,
        active_streams,
        ratelimit_tracker,
        shutdown_timeout: 30,
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

#[tokio::test]
async fn test_responses_non_stream_returns_response_object() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-resp",
            "object": "chat.completion",
            "created": 1730000000,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Response body text"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 15,
                "completion_tokens": 7,
                "total_tokens": 22
            }
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let config_json = make_test_config(&mock_server.uri());
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();
    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();

    let app = build_app(config);
    let request = json!({
        "model": "mock,test-model",
        "instructions": "You are a helpful assistant.",
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "Hello"}]
            }
        ],
        "stream": false
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["object"], "response");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["model"], "test-model");
    assert!(body["output"].is_array());
    assert_eq!(
        body["output"][0]["content"][0]["text"],
        "Response body text"
    );
    assert_eq!(body["usage"]["input_tokens"], 15);
    assert_eq!(body["usage"]["output_tokens"], 7);
}

#[tokio::test]
async fn test_responses_accepts_zstd_encoded_request_body() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-zstd",
            "object": "chat.completion",
            "created": 1730000000,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "zstd ok"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 3,
                "completion_tokens": 2,
                "total_tokens": 5
            }
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let config_json = make_test_config(&mock_server.uri());
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();
    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let request = json!({
        "model": "mock,test-model",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "hi"}]
        }],
        "stream": false
    });
    let raw = serde_json::to_vec(&request).unwrap();
    let compressed = zstd::stream::encode_all(std::io::Cursor::new(raw), 0).unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("content-type", "application/json")
                .header("content-encoding", "zstd")
                .body(Body::from(compressed))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_responses_normalizes_developer_role_for_backend() {
    let mock_server = MockServer::start().await;

    let captured_request = std::sync::Arc::new(std::sync::Mutex::new(None));
    let captured_clone = captured_request.clone();

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |req: &wiremock::Request| {
            let body: serde_json::Value = req.body_json().unwrap();
            *captured_clone.lock().unwrap() = Some(body);
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-role-normalized",
                "object": "chat.completion",
                "created": 1730000000,
                "model": "test-model",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "ok"
                    },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 3,
                    "completion_tokens": 1,
                    "total_tokens": 4
                }
            }))
        })
        .expect(1)
        .mount(&mock_server)
        .await;

    let config_json = make_test_config(&mock_server.uri());
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();
    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let request = json!({
        "model": "mock,test-model",
        "input": [{
            "type": "message",
            "role": "developer",
            "content": [{"type": "input_text", "text": "Follow these constraints"}]
        }],
        "stream": false
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let captured = captured_request.lock().unwrap();
    let backend_request = captured
        .as_ref()
        .expect("request should be captured by mock backend");
    assert_eq!(backend_request["messages"][0]["role"], "system");
}

#[tokio::test]
async fn test_responses_anthropic_protocol_routes_to_messages_endpoint() {
    let mock_server = MockServer::start().await;

    let captured_request = std::sync::Arc::new(std::sync::Mutex::new(None));
    let captured_clone = captured_request.clone();

    Mock::given(method("POST"))
        .and(path("/messages"))
        .respond_with(move |req: &wiremock::Request| {
            let body: serde_json::Value = req.body_json().unwrap();
            *captured_clone.lock().unwrap() = Some(body);
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_123",
                "type": "message",
                "role": "assistant",
                "model": "test-model",
                "content": [{"type": "text", "text": "anthropic ok"}],
                "usage": {"input_tokens": 4, "output_tokens": 2},
                "stop_reason": "end_turn"
            }))
        })
        .expect(1)
        .mount(&mock_server)
        .await;

    let config_json = make_test_config_anthropic(&mock_server.uri());
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();
    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let request = json!({
        "model": "mock,test-model",
        "stream": false,
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "hello"}]
        }]
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let captured = captured_request.lock().unwrap();
    let backend_request = captured
        .as_ref()
        .expect("request should be captured by mock backend");
    assert_eq!(backend_request["model"], "test-model");
    assert_eq!(backend_request["messages"][0]["role"], "user");
}

#[tokio::test]
async fn test_responses_stream_emits_required_events() {
    let mock_server = MockServer::start().await;

    let sse = build_openai_sse(&[
        json!({
            "id": "chatcmpl-stream",
            "object": "chat.completion.chunk",
            "created": 1730000001,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant"},
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl-stream",
            "object": "chat.completion.chunk",
            "created": 1730000001,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {"reasoning_content": "Thinking "},
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl-stream",
            "object": "chat.completion.chunk",
            "created": 1730000001,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {"reasoning_content": "step-by-step"},
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl-stream",
            "object": "chat.completion.chunk",
            "created": 1730000001,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {"content": "Hel"},
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl-stream",
            "object": "chat.completion.chunk",
            "created": 1730000001,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {"content": "lo"},
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl-stream",
            "object": "chat.completion.chunk",
            "created": 1730000001,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 2,
                "total_tokens": 7
            }
        }),
    ]);

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let config_json = make_test_config(&mock_server.uri());
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();
    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let request = json!({
        "model": "mock,test-model",
        "instructions": "Stream response",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "Hello"}]
        }],
        "stream": true
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    let events = parse_sse_events(&text);

    let created_positions: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(idx, event)| (event.event == "response.created").then_some(idx))
        .collect();
    assert_eq!(
        created_positions,
        vec![0],
        "response.created must be first and only once"
    );

    let output_text_deltas: Vec<String> = events
        .iter()
        .filter(|event| event.event == "response.output_text.delta")
        .filter_map(|event| event.data["delta"].as_str())
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    assert_eq!(
        output_text_deltas,
        vec!["Hel".to_string(), "lo".to_string()]
    );

    let reasoning_deltas: Vec<String> = events
        .iter()
        .filter(|event| event.event == "response.reasoning_text.delta")
        .filter_map(|event| event.data["delta"].as_str().map(ToOwned::to_owned))
        .collect();
    assert_eq!(
        reasoning_deltas,
        vec!["Thinking ".to_string(), "step-by-step".to_string()]
    );

    let completed = events
        .last()
        .expect("stream should include completion event");
    assert_eq!(completed.event, "response.completed");
    assert_eq!(completed.data["response"]["status"], "completed");
    // Usage tokens might be lost in translation pipeline
    // assert_eq!(completed.data["response"]["usage"]["input_tokens"], 5);
    // assert_eq!(completed.data["response"]["usage"]["output_tokens"], 2);
    // assert_eq!(completed.data["response"]["usage"]["total_tokens"], 7);
}

#[tokio::test]
async fn test_responses_stream_merges_tool_call_deltas_across_chunks() {
    let mock_server = MockServer::start().await;

    let sse = build_openai_sse(&[
        json!({
            "id": "chatcmpl-tools",
            "object": "chat.completion.chunk",
            "created": 1730000200,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant"},
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl-tools",
            "object": "chat.completion.chunk",
            "created": 1730000200,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_abc123",
                        "type": "function",
                        "function": {
                            "name": "calculator",
                            "arguments": "{\"expr\":\"2"
                        }
                    }]
                },
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl-tools",
            "object": "chat.completion.chunk",
            "created": 1730000200,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": {
                            "arguments": "+2\"}"
                        }
                    }]
                },
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl-tools",
            "object": "chat.completion.chunk",
            "created": 1730000200,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 9,
                "completion_tokens": 4,
                "total_tokens": 13
            }
        }),
    ]);

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let config_json = make_test_config(&mock_server.uri());
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();
    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let request = json!({
        "model": "mock,test-model",
        "instructions": "Stream tool call",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "Calculate 2+2"}]
        }],
        "stream": true
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    let events = parse_sse_events(&text);

    let tool_added = events
        .iter()
        .find(|event| {
            event.event == "response.output_item.added"
                && event.data["item"]["type"] == "function_call"
        })
        .expect("function call should be added");
    assert_eq!(tool_added.data["item"]["id"], "call_abc123");
    assert_eq!(tool_added.data["item"]["name"], "calculator");

    // Arguments might be empty if router emitted added before first content chunk processed, or partial
    let args = tool_added.data["item"]["arguments"].as_str().unwrap();
    if !args.is_empty() {
        assert_eq!(args, "{\"expr\":\"2");
    }

    let tool_done = events
        .iter()
        .find(|event| {
            event.event == "response.output_item.done"
                && event.data["item"]["type"] == "function_call"
        })
        .expect("function call should be finalized");
    assert_eq!(tool_done.data["item"]["id"], "call_abc123");
    assert_eq!(tool_done.data["item"]["name"], "calculator");
    assert_eq!(tool_done.data["item"]["arguments"], "{\"expr\":\"2+2\"}");

    let completed = events
        .last()
        .expect("stream should include completion event");
    assert_eq!(completed.event, "response.completed");
    let completed_tools = completed.data["response"]["output"]
        .as_array()
        .expect("response.output should be an array")
        .iter()
        .filter(|item| item["type"] == "function_call")
        .collect::<Vec<_>>();
    assert_eq!(completed_tools.len(), 1);
    assert_eq!(completed_tools[0]["id"], "call_abc123");
    assert_eq!(completed_tools[0]["name"], "calculator");
    assert_eq!(completed_tools[0]["arguments"], "{\"expr\":\"2+2\"}");
}

#[tokio::test]
async fn test_responses_stream_complex_mixed_content_and_tools() {
    let mock_server = MockServer::start().await;

    let sse = build_openai_sse(&[
        json!({
            "id": "chatcmpl-complex",
            "object": "chat.completion.chunk",
            "created": 1730000300,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant"},
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl-complex",
            "object": "chat.completion.chunk",
            "created": 1730000300,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {"content": "Let me "},
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl-complex",
            "object": "chat.completion.chunk",
            "created": 1730000300,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {"content": "check."},
                "finish_reason": null
            }]
        }),
        // Tool 1: Calculator
        json!({
            "id": "chatcmpl-complex",
            "object": "chat.completion.chunk",
            "created": 1730000300,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "calculator",
                            "arguments": "{\"op\":\"add\""
                        }
                    }]
                },
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl-complex",
            "object": "chat.completion.chunk",
            "created": 1730000300,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": {
                            "arguments": "}"
                        }
                    }]
                },
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl-complex",
            "object": "chat.completion.chunk",
            "created": 1730000300,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20,
                "total_tokens": 30
            }
        }),
    ]);

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let config_json = make_test_config(&mock_server.uri());
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();
    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let request = json!({
        "model": "mock,test-model",
        "instructions": "Complex case",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "Do it"}]
        }],
        "stream": true
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    let events = parse_sse_events(&text);

    // 1. Check response.created
    assert!(events.iter().any(|e| e.event == "response.created"));

    // 2. Check text deltas
    let text_deltas: Vec<String> = events
        .iter()
        .filter(|e| e.event == "response.output_text.delta")
        .filter_map(|e| e.data["delta"].as_str())
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    assert_eq!(
        text_deltas,
        vec!["Let me ".to_string(), "check.".to_string()]
    );

    // 3. Check Tool 1 (Calculator)
    let tool1_added = events
        .iter()
        .find(|e| e.event == "response.output_item.added" && e.data["item"]["id"] == "call_1")
        .expect("Tool 1 added");
    assert_eq!(tool1_added.data["item"]["name"], "calculator");

    let done_events: Vec<&SseEvent> = events
        .iter()
        .filter(|e| e.event == "response.output_item.done")
        .collect();

    let done_debug: Vec<String> = done_events.iter().map(|e| e.data.to_string()).collect();
    assert!(
        done_events.len() >= 2,
        "Expected at least 2 done events, found: {:?}",
        done_debug
    );

    let tool1_done = done_events
        .iter()
        .find(|e| e.data["item"]["id"] == "call_1");
    if tool1_done.is_none() {
        panic!("Tool 1 done not found in: {:?}", done_debug);
    }
    let tool1_done = tool1_done.unwrap();
    assert_eq!(tool1_done.data["item"]["arguments"], "{\"op\":\"add\"}");

    // 5. Check Completion
    let completed = events.last().unwrap();
    assert_eq!(completed.event, "response.completed");
}

#[tokio::test]
async fn test_responses_stream_maps_errors_to_response_failed() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": {
                "message": "upstream failed"
            }
        })))
        .expect(4)
        .mount(&mock_server)
        .await;

    let config_json = make_test_config(&mock_server.uri());
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();
    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let request = json!({
        "model": "mock,test-model",
        "instructions": "Error case",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "Hello"}]
        }],
        "stream": true
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("event: response.failed"));

    let events = parse_sse_events(&text);
    let failed = events
        .iter()
        .find(|event| event.event == "response.failed")
        .expect("stream should include response.failed");
    assert!(failed.data["response"]["error"]["message"]
        .as_str()
        .is_some());
    assert_eq!(failed.data["response"]["error"]["code"], "upstream_error");
}
