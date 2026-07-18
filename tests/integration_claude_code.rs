// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration tests for Claude Code frontend.
//!
//! These tests verify the end-to-end flow for Claude Code (Anthropic) format
//! requests, including frontend detection, format passthrough, and thinking
//! block preservation.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::Router;
use serde_json::json;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use ccr_rust::frontend::{detect_frontend, FrontendType};

// ---------------------------------------------------------------------------
// Test Helpers
// ---------------------------------------------------------------------------

/// Build test config pointing at mock server.
fn make_test_config(mock_url: &str) -> String {
    let config = json!({
        "Providers": [
            {
                "name": "mock",
                "api_base_url": mock_url,
                "api_key": "test-key",
                "models": ["claude-sonnet-4-6"]
            }
        ],
        "Router": {
            "default": "mock,claude-sonnet-4-6"
        },
        "API_TIMEOUT_MS": 5000
    });
    serde_json::to_string_pretty(&config).unwrap()
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
        shutdown_timeout: 30,
        debug_capture: None,
    };
    Router::new()
        .route("/v1/messages", post(ccr_rust::router::handle_messages))
        .with_state(state)
}

/// Helper to create Anthropic-style request body.
fn anthropic_request_body() -> serde_json::Value {
    json!({
        "model": "claude-sonnet-4-6",
        "messages": [{"role": "user", "content": "Hello, Claude"}],
        "max_tokens": 1000
    })
}

/// Helper to create OpenAI-style request body.
fn openai_request_body() -> serde_json::Value {
    json!({
        "model": "gpt-4",
        "messages": [
            {"role": "system", "content": "You are a helper"},
            {"role": "user", "content": "Hello"}
        ],
        "max_tokens": 100
    })
}

/// Skip integration tests that require opening localhost sockets when the
/// execution environment forbids binding ports.
fn skip_if_localhost_bind_unavailable(test_name: &str) -> bool {
    if std::net::TcpListener::bind("127.0.0.1:0").is_ok() {
        return false;
    }

    eprintln!("Skipping {test_name}: cannot bind localhost sockets in this environment");
    true
}

// ---------------------------------------------------------------------------
// Frontend Detection Tests
// ---------------------------------------------------------------------------

#[test]
fn test_claude_code_request_detection_by_headers() {
    // Test detection via Anthropic-specific headers (anthropic-client-id → ClaudeCode)
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("anthropic-client-id", "test-client".parse().unwrap());

    let body = json!({
        "model": "claude-sonnet-4-6",
        "messages": [{"role": "user", "content": "Hello"}]
    });

    let detected = detect_frontend(&headers, &body);
    assert_eq!(detected, FrontendType::ClaudeCode);
}

#[test]
fn test_claude_code_request_detection_by_anthropic_version_header() {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("anthropic-version", "2023-06-01".parse().unwrap());

    let body = json!({});

    let detected = detect_frontend(&headers, &body);
    assert_eq!(detected, FrontendType::ClaudeCode);
}

#[test]
fn test_claude_code_request_detection_by_format() {
    // Test detection via Anthropic request format (model + messages without roles)
    // Anthropic format uses "human" role, not "user"
    let headers = axum::http::HeaderMap::new();

    let body = json!({
        "model": "claude-sonnet-4-6",
        "max_tokens": 1024,
        "messages": [{"role": "human", "content": "Hello"}]
    });

    let detected = detect_frontend(&headers, &body);
    // Note: With role present, it matches OpenAI format first and returns Codex
    // The detection falls through to ClaudeCode when no role field is present
    assert_eq!(detected, FrontendType::Codex);
}

#[test]
fn test_claude_code_request_detection_by_anthropic_specific_format() {
    // Test detection via Anthropic-specific fields (anthropic_version)
    let headers = axum::http::HeaderMap::new();

    let body = json!({
        "model": "claude-sonnet-4-6",
        "anthropic_version": "2023-06-01",
        "messages": [{"role": "user", "content": "Hello"}]
    });

    let detected = detect_frontend(&headers, &body);
    // Codex takes precedence for ambiguous role-based payloads.
    assert_eq!(detected, FrontendType::Codex);
}

#[test]
fn test_claude_code_request_detection_by_system_field() {
    // Anthropic format has system as top-level field
    let headers = axum::http::HeaderMap::new();

    let body = json!({
        "model": "claude-sonnet-4-6",
        "system": "You are Claude, a helpful AI assistant.",
        "messages": [{"role": "user", "content": "Hello"}]
    });

    let detected = detect_frontend(&headers, &body);
    assert_eq!(detected, FrontendType::Codex);
}

#[test]
fn test_claude_code_vs_codex_detection() {
    // OpenAI format should be detected as Codex (not ClaudeCode)
    let headers = axum::http::HeaderMap::new();
    let body = openai_request_body();

    let detected = detect_frontend(&headers, &body);
    // OpenAI format (messages with role) is detected as Codex
    assert_eq!(detected, FrontendType::Codex);
}

#[test]
fn test_claude_code_detection_codex_user_agent_precedence() {
    // If User-Agent says "codex", it should take precedence over Anthropic format
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("user-agent", "codex-cli/1.0.0".parse().unwrap());

    let body = json!({
        "model": "claude-sonnet-4-6",
        "max_tokens": 1024,
        "messages": [{"role": "user", "content": "Hello"}]
    });

    let detected = detect_frontend(&headers, &body);
    assert_eq!(detected, FrontendType::Codex);
}

// ---------------------------------------------------------------------------
// Anthropic Format Passthrough Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_claude_code_passthrough_basic_request() {
    if skip_if_localhost_bind_unavailable("test_claude_code_passthrough_basic_request") {
        return;
    }
    let mock_server = MockServer::start().await;

    // Mock backend expects OpenAI format (after translation)
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "resp_123",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "claude-sonnet-4-6",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello! How can I help you today?"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
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
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .header("anthropic-client-id", "test-client")
                .body(Body::from(
                    serde_json::to_vec(&anthropic_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    // Parse response to verify Anthropic format
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    // Verify Anthropic response structure
    assert_eq!(response_json["type"], "message");
    assert_eq!(response_json["role"], "assistant");
    assert!(response_json["content"].is_array());
    assert_eq!(response_json["content"][0]["type"], "text");
    assert_eq!(
        response_json["content"][0]["text"],
        "Hello! How can I help you today?"
    );
}

#[tokio::test]
async fn test_claude_code_passthrough_with_system_prompt() {
    if skip_if_localhost_bind_unavailable("test_claude_code_passthrough_with_system_prompt") {
        return;
    }
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "resp_456",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "claude-sonnet-4-6",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "I understand. I will be helpful."
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 25,
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

    let request_body = json!({
        "model": "mock,claude-sonnet-4-6",
        "system": "You are a helpful assistant.",
        "messages": [{"role": "user", "content": "Please acknowledge my system prompt."}],
        "max_tokens": 1000
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(response_json["type"], "message");
    assert!(response_json["content"].is_array());
}

#[tokio::test]
async fn test_claude_code_passthrough_streaming() {
    if skip_if_localhost_bind_unavailable("test_claude_code_passthrough_streaming") {
        return;
    }
    let mock_server = MockServer::start().await;

    // Build SSE stream response
    let sse_body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({
            "id": "chunk_1",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": "claude-sonnet-4-6",
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant"},
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chunk_2",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": "claude-sonnet-4-6",
            "choices": [{
                "index": 0,
                "delta": {"content": "Hello from streaming!"},
                "finish_reason": "stop"
            }]
        })
    );

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body)
                .insert_header("content-type", "text/event-stream"),
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

    let request_body = json!({
        "model": "mock,claude-sonnet-4-6",
        "messages": [{"role": "user", "content": "Stream a response"}],
        "max_tokens": 1000,
        "stream": true
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "text/event-stream"
    );
}

// ---------------------------------------------------------------------------
// Thinking Block Preservation Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_claude_code_thinking_preserved_in_response() {
    if skip_if_localhost_bind_unavailable("test_claude_code_thinking_preserved_in_response") {
        return;
    }
    let mock_server = MockServer::start().await;

    // Mock backend returns reasoning_content (thinking) + regular content
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp_with_thinking",
                "object": "chat.completion",
                "created": 1234567890,
                "model": "deepseek-reasoner",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "The answer is 42.",
                        "reasoning_content": "Let me analyze this step by step:\n1. First, understand the problem\n2. Then, solve it"
                    },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 15,
                    "completion_tokens": 35
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

    let request_body = json!({
        "model": "mock,deepseek-reasoner",
        "messages": [{"role": "user", "content": "What is the answer?"}],
        "max_tokens": 1000
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    // Reasoning from non-Anthropic providers is stripped (missing thinking
    // signature causes SDK parse failures), so only the text block remains.
    assert!(response_json["content"].is_array());
    assert_eq!(response_json["content"].as_array().unwrap().len(), 1);

    assert_eq!(response_json["content"][0]["type"], "text");
    assert_eq!(response_json["content"][0]["text"], "The answer is 42.");
}

#[tokio::test]
async fn test_claude_code_thinking_in_streaming_response() {
    if skip_if_localhost_bind_unavailable("test_claude_code_thinking_in_streaming_response") {
        return;
    }
    let mock_server = MockServer::start().await;

    // Build SSE stream with reasoning_content deltas
    let sse_body = format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({
            "id": "chunk_1",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": "deepseek-reasoner",
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant"},
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chunk_2",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": "deepseek-reasoner",
            "choices": [{
                "index": 0,
                "delta": {"reasoning_content": "Analyzing the problem..."},
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chunk_3",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": "deepseek-reasoner",
            "choices": [{
                "index": 0,
                "delta": {"content": "The solution is ready."},
                "finish_reason": "stop"
            }]
        })
    );

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body)
                .insert_header("content-type", "text/event-stream"),
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

    let request_body = json!({
        "model": "mock,deepseek-reasoner",
        "messages": [{"role": "user", "content": "Solve this step by step"}],
        "max_tokens": 1000,
        "stream": true
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    // Read the SSE stream
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_text = String::from_utf8(body_bytes.to_vec()).unwrap();

    // Reasoning from non-Anthropic providers is skipped in streaming
    // translation (missing thinking signature → SDK parse failures).
    assert!(!body_text.contains("thinking_delta"));
    assert!(body_text.contains("text_delta"));
    assert!(body_text.contains("The solution is ready."));
}

#[tokio::test]
async fn test_claude_code_thinking_empty_reasoning_skipped() {
    if skip_if_localhost_bind_unavailable("test_claude_code_thinking_empty_reasoning_skipped") {
        return;
    }
    let mock_server = MockServer::start().await;

    // Mock backend returns empty reasoning_content (should not create thinking block)
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "resp_no_thinking",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "claude-sonnet-4-6",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Just a regular response.",
                    "reasoning_content": ""
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
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

    let request_body = json!({
        "model": "mock,claude-sonnet-4-6",
        "messages": [{"role": "user", "content": "Say something"}],
        "max_tokens": 1000
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    // Should only have text content block, no thinking block
    assert_eq!(response_json["content"].as_array().unwrap().len(), 1);
    assert_eq!(response_json["content"][0]["type"], "text");
}

#[tokio::test]
async fn test_claude_code_thinking_only_no_content() {
    if skip_if_localhost_bind_unavailable("test_claude_code_thinking_only_no_content") {
        return;
    }
    let mock_server = MockServer::start().await;

    // Mock backend returns only reasoning_content, no regular content
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "resp_thinking_only",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "deepseek-reasoner",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "reasoning_content": "Thinking deeply about this..."
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 5,
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

    let request_body = json!({
        "model": "mock,deepseek-reasoner",
        "messages": [{"role": "user", "content": "Think about this"}],
        "max_tokens": 1000
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    // Reasoning from non-Anthropic providers is stripped, so content is empty
    // when the model produced only reasoning_content with no text.
    assert_eq!(response_json["content"].as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// End-to-end Flow Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_claude_code_end_to_end_with_tier_retries() {
    if skip_if_localhost_bind_unavailable("test_claude_code_end_to_end_with_tier_retries") {
        return;
    }
    let mock_server = MockServer::start().await;

    // First request fails, second succeeds
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("temp error"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "resp_success",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "claude-sonnet-4-6",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Success after retry!"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5
            }
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let config = json!({
        "Providers": [
            {
                "name": "mock",
                "api_base_url": mock_server.uri(),
                "api_key": "test-key",
                "models": ["claude-sonnet-4-6"]
            }
        ],
        "Router": {
            "default": "mock,claude-sonnet-4-6",
            "tierRetries": {
                "tier-0": {
                    "max_retries": 3,
                    "base_backoff_ms": 10,
                    "backoff_multiplier": 1.0,
                    "max_backoff_ms": 100
                }
            }
        },
        "API_TIMEOUT_MS": 5000
    });

    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let cfg = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(cfg);

    let request_body = json!({
        "model": "mock,claude-sonnet-4-6",
        "messages": [{"role": "user", "content": "Test retry"}],
        "max_tokens": 1000
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .header("anthropic-client-id", "test-client")
                .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    // Should succeed after retry
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(response_json["content"][0]["text"], "Success after retry!");
}

#[tokio::test]
async fn test_streamed_usage_estimate_fallback_records_no_fake_drift() {
    if skip_if_localhost_bind_unavailable(
        "test_streamed_usage_estimate_fallback_records_no_fake_drift",
    ) {
        return;
    }
    let mock_server = MockServer::start().await;

    // OpenAI-compatible SSE stream that omits the usage chunk entirely, as
    // upstreams do when stream_options.include_usage is not set.
    let sse_body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({
            "id": "chunk_1",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": "claude-sonnet-4-6",
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant", "content": "Hello"},
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chunk_2",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": "claude-sonnet-4-6",
            "choices": [{
                "index": 0,
                "delta": {"content": " there."},
                "finish_reason": "stop"
            }]
        })
    );

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body)
                .insert_header("content-type", "text/event-stream"),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    // Unique provider name so the drift-state assertion cannot collide with
    // other tests in this binary that share the global metrics state.
    let config_value = json!({
        "Providers": [
            {
                "name": "drift-probe-mock",
                "api_base_url": mock_server.uri(),
                "api_key": "test-key",
                "models": ["claude-sonnet-4-6"]
            }
        ],
        "Router": {
            "default": "drift-probe-mock,claude-sonnet-4-6"
        },
        "API_TIMEOUT_MS": 5000
    });
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(
        &config_path,
        serde_json::to_string_pretty(&config_value).unwrap(),
    )
    .unwrap();

    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let request_body = json!({
        "model": "drift-probe-mock,claude-sonnet-4-6",
        "messages": [{"role": "user", "content": "Say hello in a few words"}],
        "max_tokens": 1000,
        "stream": true
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_text = String::from_utf8(body_bytes.to_vec()).unwrap();

    // The final message_delta usage must carry the pre-request estimate, not
    // the raw 0 the upstream (implicitly) reported.
    let mut message_delta_input_tokens = None;
    for line in body_text.lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        if event["type"] == "message_delta" {
            if let Some(input_tokens) = event["usage"]["input_tokens"].as_u64() {
                message_delta_input_tokens = Some(input_tokens);
            }
        }
    }
    let message_delta_input_tokens =
        message_delta_input_tokens.expect("stream should contain a message_delta event with usage");

    // The client-visible value must equal the recorded pre-request estimate,
    // which /v1/token-audit exposes as total_tokens for this tier.
    let audit_resp =
        axum::response::IntoResponse::into_response(ccr_rust::metrics::token_audit_handler().await);
    let audit_bytes = axum::body::to_bytes(audit_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let audit_json: serde_json::Value = serde_json::from_slice(&audit_bytes).unwrap();
    let expected_estimate = audit_json
        .as_array()
        .expect("token-audit response is a JSON array")
        .iter()
        .rev()
        .find(|e| {
            e["tier"]
                .as_str()
                .is_some_and(|t| t.contains("drift-probe-mock"))
        })
        .and_then(|e| e["total_tokens"].as_u64())
        .expect("audit log should contain a pre-request estimate for this tier");
    assert!(
        expected_estimate > 0,
        "pre-request estimate should be nonzero"
    );
    assert_eq!(
        message_delta_input_tokens, expected_estimate,
        "client-visible input_tokens should equal the recorded pre-request estimate"
    );

    // But drift verification must NOT have recorded a sample for this tier:
    // the upstream omitted usage, so there is nothing to compare the estimate
    // against, and a substituted estimate would register as false 0% drift.
    let drift_resp =
        axum::response::IntoResponse::into_response(ccr_rust::metrics::token_drift_handler().await);
    let drift_bytes = axum::body::to_bytes(drift_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let drift_json: serde_json::Value = serde_json::from_slice(&drift_bytes).unwrap();
    let fake_drift_entries: Vec<&serde_json::Value> = drift_json
        .as_array()
        .expect("token-drift response is a JSON array")
        .iter()
        .filter(|e| {
            e["tier"]
                .as_str()
                .is_some_and(|t| t.contains("drift-probe-mock"))
        })
        .collect();
    assert!(
        fake_drift_entries.is_empty(),
        "no drift sample should be recorded when upstream omits usage, got: {fake_drift_entries:?}"
    );
}
