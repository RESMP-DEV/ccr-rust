//! Integration tests for Codex frontend.
//!
//! Tests the end-to-end flow for OpenAI Codex CLI clients:
//! - Frontend detection based on User-Agent and request format
//! - Request transformation: OpenAI -> Internal (Anthropic) -> OpenAI
//! - Response transformation: OpenAI -> Anthropic -> OpenAI

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use serde_json::json;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ============================================================================
// Test Fixtures and Helpers
// ============================================================================

/// Build an OpenAI-style request body (Codex format)
fn codex_request_body() -> serde_json::Value {
    json!({
        "model": "mock,test-model",
        "messages": [
            {"role": "system", "content": "You are a helpful assistant."},
            {"role": "user", "content": "Hello, world!"}
        ],
        "max_tokens": 100,
        "temperature": 0.7
    })
}

/// Helper: Create test config with mock backend
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

/// Build the Axum app with all necessary state
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
        debug_capture: None,
    };

    // Register both Anthropic and OpenAI endpoints
    Router::new()
        .route("/v1/messages", post(ccr_rust::router::handle_messages))
        .route(
            "/v1/chat/completions",
            post(ccr_rust::router::handle_chat_completions),
        )
        .route("/v1/models", get(ccr_rust::router::list_models))
        .route(
            "/v1/frontend-metrics",
            get(ccr_rust::metrics::frontend_metrics_handler),
        )
        .with_state(state)
}

/// Skip integration tests that require opening localhost sockets when the
/// execution environment forbids binding ports.
fn skip_if_localhost_bind_unavailable() -> bool {
    if std::net::TcpListener::bind("127.0.0.1:0").is_ok() {
        return false;
    }

    eprintln!("Skipping test: cannot bind localhost sockets in this environment");
    true
}

// ============================================================================
// Test: Frontend Detection
// ============================================================================

#[tokio::test]
async fn test_codex_request_detection_by_user_agent() {
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let mock_server = MockServer::start().await;

    // Mock the backend to expect OpenAI format
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello! How can I help you today?"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 20,
                "completion_tokens": 10
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

    // Send request with Codex User-Agent
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("user-agent", "codex-cli/1.0.0")
                .body(Body::from(
                    serde_json::to_vec(&codex_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    // Verify response is in OpenAI format
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    // Should be OpenAI format response
    assert!(response_json.get("choices").is_some());
    assert_eq!(
        response_json["choices"][0]["message"]["content"],
        "Hello! How can I help you today?"
    );
}

#[tokio::test]
async fn test_models_endpoint_includes_route_ids_for_codex() {
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let mock_server = MockServer::start().await;

    let config_json = make_test_config(&mock_server.uri());
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();

    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(response_json["object"], "list");
    assert!(response_json["models"].is_array());
    let data = response_json["data"].as_array().unwrap();
    let ids: Vec<String> = data
        .iter()
        .filter_map(|item| item["id"].as_str().map(|s| s.to_string()))
        .collect();

    assert!(ids.contains(&"mock,test-model".to_string()));
    assert!(ids.contains(&"test-model".to_string()));
}

#[tokio::test]
async fn test_frontend_metrics_records_codex_requests() {
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-metrics",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "metrics ok"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 3,
                "total_tokens": 8
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

    let _resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("user-agent", "codex-cli/1.0.0")
                .body(Body::from(
                    serde_json::to_vec(&codex_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/frontend-metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let metrics: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    let entries = metrics.as_array().unwrap();
    let codex = entries
        .iter()
        .find(|entry| entry.get("frontend").and_then(|v| v.as_str()) == Some("codex"))
        .expect("expected codex frontend metrics entry");
    assert!(codex["requests"].as_u64().unwrap_or(0) >= 1);
}

#[tokio::test]
async fn test_codex_request_detection_by_format() {
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let mock_server = MockServer::start().await;

    // Mock the backend
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Response from format detection"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 15,
                "completion_tokens": 8
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

    // Send request with OpenAI format but no specific User-Agent
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&codex_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    // Verify response
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    assert!(response_json.get("choices").is_some());
    assert_eq!(
        response_json["choices"][0]["message"]["content"],
        "Response from format detection"
    );
}

// ============================================================================
// Test: Request Transformation (OpenAI -> Internal -> OpenAI)
// ============================================================================

#[tokio::test]
async fn test_codex_request_transformation_openai_to_internal() {
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let mock_server = MockServer::start().await;

    // Capture the request that reaches the backend
    let captured_request = std::sync::Arc::new(std::sync::Mutex::new(None));
    let captured_clone = captured_request.clone();

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |req: &wiremock::Request| {
            // Capture the request body
            let body: serde_json::Value = req.body_json().unwrap();
            *captured_clone.lock().unwrap() = Some(body);

            ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-captured",
                "object": "chat.completion",
                "created": 1234567890,
                "model": "test-model",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Captured!"
                    },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 5
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

    // Send OpenAI format request
    let openai_request = json!({
        "model": "mock,test-model",
        "messages": [
            {"role": "system", "content": "System prompt"},
            {"role": "user", "content": "User message"}
        ],
        "max_tokens": 100,
        "temperature": 0.5
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("user-agent", "codex-cli/1.0.0")
                .body(Body::from(serde_json::to_vec(&openai_request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    // Verify the transformed request
    let captured = captured_request.lock().unwrap();
    let backend_request = captured
        .as_ref()
        .expect("Request should have been captured");

    // Backend should receive OpenAI format (since router translates Anthropic -> OpenAI)
    assert!(backend_request.get("messages").is_some());
    assert!(backend_request.get("model").is_some());
    assert_eq!(backend_request["messages"][0]["role"], "system");
    assert_eq!(backend_request["messages"][0]["content"], "System prompt");
    assert_eq!(backend_request["messages"][1]["role"], "user");
    assert_eq!(backend_request["messages"][1]["content"], "User message");
    assert_eq!(backend_request["max_tokens"], 100);
    assert_eq!(backend_request["temperature"], 0.5);
}

#[tokio::test]
async fn test_codex_request_transformation_with_tools() {
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let mock_server = MockServer::start().await;

    // Capture the request
    let captured_request = std::sync::Arc::new(std::sync::Mutex::new(None));
    let captured_clone = captured_request.clone();

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |req: &wiremock::Request| {
            let body: serde_json::Value = req.body_json().unwrap();
            *captured_clone.lock().unwrap() = Some(body);

            ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-tools",
                "object": "chat.completion",
                "created": 1234567890,
                "model": "test-model",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "I'll help you calculate that."
                    },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 25,
                    "completion_tokens": 8
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

    // Send OpenAI request with tools
    let openai_request = json!({
        "model": "mock,test-model",
        "messages": [
            {"role": "user", "content": "Calculate 2+2"}
        ],
        "tools": [
            {
                "type": "function",
                "function": {
                    "name": "calculator",
                    "description": "Perform calculations",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "expression": {"type": "string"}
                        }
                    }
                }
            }
        ],
        "tool_choice": "auto",
        "max_tokens": 100
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("user-agent", "codex-cli/1.0.0")
                .body(Body::from(serde_json::to_vec(&openai_request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    // Verify tools were passed through correctly
    let captured = captured_request.lock().unwrap();
    let backend_request = captured
        .as_ref()
        .expect("Request should have been captured");

    assert!(backend_request.get("tools").is_some());
    assert_eq!(backend_request["tools"][0]["type"], "function");
    assert_eq!(
        backend_request["tools"][0]["function"]["name"],
        "calculator"
    );
}

// ============================================================================
// Test: Response Transformation (OpenAI -> Anthropic -> OpenAI)
// ============================================================================

#[tokio::test]
async fn test_codex_response_transformation_simple() {
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let mock_server = MockServer::start().await;

    // Mock OpenAI format response from backend
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-abc123",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "This is the response content."
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 30,
                "completion_tokens": 15
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

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("user-agent", "codex-cli/1.0.0")
                .body(Body::from(
                    serde_json::to_vec(&codex_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    // Verify the response is in proper OpenAI format
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    // OpenAI format checks
    assert!(response_json.get("id").is_some());
    assert!(response_json.get("object").is_some());
    assert!(response_json.get("created").is_some());
    assert!(response_json.get("model").is_some());
    assert!(response_json.get("choices").is_some());
    assert!(response_json.get("usage").is_some());

    // Check choices array structure
    let choices = response_json["choices"].as_array().unwrap();
    assert!(!choices.is_empty());
    assert!(choices[0].get("index").is_some());
    assert!(choices[0].get("message").is_some());
    assert!(choices[0].get("finish_reason").is_some());

    // Check message structure
    let message = &choices[0]["message"];
    assert_eq!(message["role"], "assistant");
    assert_eq!(message["content"], "This is the response content.");

    // Check usage structure
    let usage = &response_json["usage"];
    assert!(usage.get("prompt_tokens").is_some());
    assert!(usage.get("completion_tokens").is_some());
}

#[tokio::test]
async fn test_codex_response_transformation_with_tool_calls() {
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let mock_server = MockServer::start().await;

    // Mock OpenAI format response with tool calls
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-tool123",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "I'll calculate that for you.",
                    "tool_calls": [
                        {
                            "id": "call_abc123",
                            "type": "function",
                            "function": {
                                "name": "calculator",
                                "arguments": "{\"expression\": \"2+2\"}"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 40,
                "completion_tokens": 20
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

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("user-agent", "codex-cli/1.0.0")
                .body(Body::from(
                    serde_json::to_vec(&codex_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    // Verify tool calls are preserved in OpenAI format
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    let message = &response_json["choices"][0]["message"];
    assert!(message.get("tool_calls").is_some());

    let tool_calls = message["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0]["id"], "call_abc123");
    assert_eq!(tool_calls[0]["type"], "function");
    assert_eq!(tool_calls[0]["function"]["name"], "calculator");
    let args = tool_calls[0]["function"]["arguments"].as_str().unwrap();
    let args_json: serde_json::Value = serde_json::from_str(args).unwrap();
    assert_eq!(args_json, json!({"expression": "2+2"}));

    assert_eq!(response_json["choices"][0]["finish_reason"], "tool_calls");
}

#[tokio::test]
async fn test_codex_response_transformation_max_tokens() {
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let mock_server = MockServer::start().await;

    // Mock response with length finish_reason
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-maxtok",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "This response was cut off because..."
                },
                "finish_reason": "length"
            }],
            "usage": {
                "prompt_tokens": 50,
                "completion_tokens": 100
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

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("user-agent", "codex-cli/1.0.0")
                .body(Body::from(
                    serde_json::to_vec(&codex_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    // Verify finish_reason is preserved as "length"
    assert_eq!(response_json["choices"][0]["finish_reason"], "length");
}

// ============================================================================
// Test: End-to-End Flow Verification
// ============================================================================

#[tokio::test]
async fn test_codex_end_to_end_full_flow() {
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let mock_server = MockServer::start().await;

    // Set up mock that validates the full transformation chain
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-e2e",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "End-to-end test successful!"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 25,
                "completion_tokens": 12,
                "total_tokens": 37
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

    // Full OpenAI-style request
    let request = json!({
        "model": "mock,test-model",
        "messages": [
            {"role": "system", "content": "You are a helpful assistant."},
            {"role": "user", "content": "Test the end-to-end flow."}
        ],
        "max_tokens": 100,
        "temperature": 0.7,
        "top_p": 1.0,
        "n": 1,
        "stream": false
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("user-agent", "codex-cli/1.0.0")
                .header("authorization", "Bearer test-key")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    // Parse and verify complete OpenAI response structure
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    // Verify all OpenAI response fields
    assert_eq!(response["id"], "chatcmpl-e2e");
    assert_eq!(response["object"], "chat.completion");
    assert!(response["created"].as_i64().is_some());
    assert_eq!(response["model"], "gpt-4");

    // Verify choices
    let choices = response["choices"].as_array().unwrap();
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0]["index"], 0);
    assert_eq!(choices[0]["message"]["role"], "assistant");
    assert_eq!(
        choices[0]["message"]["content"],
        "End-to-end test successful!"
    );
    assert_eq!(choices[0]["finish_reason"], "stop");

    // Verify usage
    assert_eq!(response["usage"]["prompt_tokens"], 25);
    assert_eq!(response["usage"]["completion_tokens"], 12);
    assert_eq!(response["usage"]["total_tokens"], 37);
}

// ============================================================================
// Test: Error Handling
// ============================================================================

#[tokio::test]
async fn test_codex_backend_error_propagation() {
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let mock_server = MockServer::start().await;

    // Mock backend returning an error
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": {
                "message": "Internal server error",
                "type": "internal_error",
                "code": "internal_error"
            }
        })))
        .expect(4) // Initial + 3 retries
        .mount(&mock_server)
        .await;

    let config_json = make_test_config(&mock_server.uri());
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();

    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("user-agent", "codex-cli/1.0.0")
                .body(Body::from(
                    serde_json::to_vec(&codex_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // Should return 503 after all retries are exhausted
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn test_codex_rate_limited_error_normalization_and_headers() {
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "13")
                .set_body_json(json!({
                    "error": {
                        "message": "too many requests"
                    }
                })),
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

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("user-agent", "codex-cli/1.0.0")
                .body(Body::from(
                    serde_json::to_vec(&codex_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        resp.headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok()),
        Some("13")
    );
    assert!(resp.headers().get("x-ccr-tier").is_some());

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(response_json["error"]["type"], "rate_limit_error");
    assert_eq!(response_json["error"]["code"], "rate_limited");
    assert_eq!(response_json["error"]["retry_after"], 13);
}

// ============================================================================
// Test: Request Format Variations
// ============================================================================

#[tokio::test]
async fn test_codex_multimodal_content() {
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-multimodal",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "I can see the image you sent."
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 10
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

    // OpenAI multimodal request
    let multimodal_request = json!({
        "model": "mock,test-model",
        "messages": [
            {
                "role": "user",
                "content": [
                    {"type": "text", "text": "What's in this image?"},
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": "https://example.com/image.jpg"
                        }
                    }
                ]
            }
        ],
        "max_tokens": 100
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("user-agent", "codex-cli/1.0.0")
                .body(Body::from(serde_json::to_vec(&multimodal_request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(
        response_json["choices"][0]["message"]["content"],
        "I can see the image you sent."
    );
}

#[tokio::test]
async fn test_codex_response_transformation_with_reasoning_content() {
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let mock_server = MockServer::start().await;

    // Mock Anthropic-style response from backend that contains thinking blocks
    // Note: The router will convert the backend response to InternalResponse
    // and then the CodexFrontend will serialize it to OpenAI format.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-5-sonnet",
            "content": [
                {
                    "type": "thinking",
                    "thinking": "Thinking about the user's request...",
                    "signature": "test-signature-abc123"
                },
                {
                    "type": "text",
                    "text": "Here is the answer."
                }
            ],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 20
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

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("user-agent", "codex-cli/1.0.0")
                .body(Body::from(
                    serde_json::to_vec(&codex_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    let message = &response_json["choices"][0]["message"];
    assert_eq!(message["content"], "Here is the answer.");
    assert_eq!(
        message["reasoning_content"],
        "Thinking about the user's request..."
    );
}

#[tokio::test]
async fn test_codex_empty_messages() {
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-empty",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "How can I help you?"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 5
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

    // Request with just a system message
    let request = json!({
        "model": "mock,test-model",
        "messages": [
            {"role": "system", "content": "You are a helpful assistant."}
        ],
        "max_tokens": 50
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("user-agent", "codex-cli/1.0.0")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}
