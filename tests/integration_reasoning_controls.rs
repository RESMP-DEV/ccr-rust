// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end regression tests for reasoning controls sent to Anthropic
//! protocol providers.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn make_anthropic_config(mock_url: &str) -> String {
    serde_json::to_string_pretty(&json!({
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
    }))
    .unwrap()
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
        .route("/v1/responses", post(ccr_rust::router::handle_responses))
        .with_state(state)
}

fn app_for_mock(mock_server: &MockServer) -> Router {
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.json");
    std::fs::write(&config_path, make_anthropic_config(&mock_server.uri())).unwrap();
    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    build_app(config)
}

async fn mount_anthropic_json_success(mock_server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_reasoning_controls",
            "type": "message",
            "role": "assistant",
            "model": "test-model",
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 4, "output_tokens": 2}
        })))
        .expect(1)
        .mount(mock_server)
        .await;
}

fn anthropic_success_sse() -> String {
    [
        r#"event: message_start"#,
        r#"data: {"type":"message_start","message":{"id":"msg_stream_controls","model":"test-model","usage":{"input_tokens":3,"output_tokens":0}}}"#,
        "",
        r#"event: content_block_start"#,
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        "",
        r#"event: content_block_delta"#,
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"streamed ok"}}"#,
        "",
        r#"event: content_block_stop"#,
        r#"data: {"type":"content_block_stop","index":0}"#,
        "",
        r#"event: message_delta"#,
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#,
        "",
        r#"event: message_stop"#,
        r#"data: {"type":"message_stop"}"#,
        "",
        "",
    ]
    .join("\n")
}

async fn mount_anthropic_sse_success(mock_server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(anthropic_success_sse()),
        )
        .expect(1)
        .mount(mock_server)
        .await;
}

async fn send_json(app: Router, endpoint: &str, request: Value) -> Response {
    let mut builder = Request::builder()
        .method("POST")
        .uri(endpoint)
        .header("content-type", "application/json");
    if endpoint == "/v1/messages" {
        builder = builder.header("anthropic-version", "2023-06-01");
    }
    app.oneshot(
        builder
            .body(Body::from(serde_json::to_vec(&request).unwrap()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn successful_json_response(response: Response) -> Value {
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

async fn captured_upstream_json(mock_server: &MockServer) -> Value {
    let requests = mock_server.received_requests().await.unwrap();
    assert_eq!(
        requests.len(),
        1,
        "the Anthropic upstream should receive exactly one request"
    );
    let request = requests.into_iter().next().unwrap();
    assert_eq!(request.url.path(), "/messages");
    assert_eq!(request.method.as_str(), "POST");
    serde_json::from_slice(&request.body).expect("upstream request should be valid JSON")
}

fn assert_no_openai_or_internal_control_keys(upstream: &Value) {
    for key in [
        "reasoning_effort",
        "reasoning",
        "__ccr_responses_request",
        "__ccr_responses_response",
    ] {
        assert!(
            upstream.get(key).is_none(),
            "Anthropic wire request unexpectedly contains {key}: {upstream}"
        );
    }
}

#[tokio::test]
async fn responses_reasoning_effort_maps_to_anthropic_output_config_nonstreaming() {
    let mock_server = MockServer::start().await;
    mount_anthropic_json_success(&mock_server).await;
    let app = app_for_mock(&mock_server);

    let response = send_json(
        app,
        "/v1/responses",
        json!({
            "model": "mock,test-model",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "hello"}]
            }],
            "max_output_tokens": 128_000,
            "reasoning": {"effort": "max", "summary": "auto"},
            "stream": false
        }),
    )
    .await;
    let response = successful_json_response(response).await;
    assert_eq!(response["object"], "response");
    assert_eq!(response["status"], "completed");

    let upstream = captured_upstream_json(&mock_server).await;
    assert_eq!(upstream["model"], "test-model");
    assert_eq!(upstream["max_tokens"], 128_000);
    assert_eq!(upstream["output_config"], json!({"effort": "max"}));
    assert_no_openai_or_internal_control_keys(&upstream);
}

#[tokio::test]
async fn chat_reasoning_effort_maps_to_anthropic_output_config_nonstreaming() {
    let mock_server = MockServer::start().await;
    mount_anthropic_json_success(&mock_server).await;
    let app = app_for_mock(&mock_server);

    let response = send_json(
        app,
        "/v1/chat/completions",
        json!({
            "model": "mock,test-model",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 128_000,
            "reasoning_effort": "high",
            "stream": false
        }),
    )
    .await;
    let response = successful_json_response(response).await;
    assert_eq!(response["choices"][0]["message"]["content"], "ok");

    let upstream = captured_upstream_json(&mock_server).await;
    assert_eq!(upstream["model"], "test-model");
    assert_eq!(upstream["max_tokens"], 128_000);
    assert_eq!(upstream["output_config"], json!({"effort": "high"}));
    assert_no_openai_or_internal_control_keys(&upstream);
}

#[tokio::test]
async fn native_messages_thinking_modes_and_output_config_are_passed_through() {
    let cases = [
        (
            "budgeted",
            json!({"type": "enabled", "budget_tokens": 12_000}),
            json!({"effort": "low", "budget_source": "client"}),
        ),
        (
            "adaptive",
            json!({"type": "adaptive"}),
            json!({"effort": "medium", "sync": "never"}),
        ),
        (
            "disabled",
            json!({"type": "disabled"}),
            json!({"effort": "minimal", "include": ["reasoning"]}),
        ),
    ];

    for (case_name, thinking, output_config) in cases {
        let mock_server = MockServer::start().await;
        mount_anthropic_json_success(&mock_server).await;
        let app = app_for_mock(&mock_server);

        let response = send_json(
            app,
            "/v1/messages",
            json!({
                "model": "mock,test-model",
                "messages": [{"role": "user", "content": "hello"}],
                "max_tokens": 128_000,
                "thinking": thinking,
                "output_config": output_config,
                "stream": false
            }),
        )
        .await;
        let response = successful_json_response(response).await;
        assert_eq!(response["content"][0]["text"], "ok", "{case_name}");

        let upstream = captured_upstream_json(&mock_server).await;
        assert_eq!(upstream["thinking"], thinking, "{case_name}");
        assert_eq!(upstream["output_config"], output_config, "{case_name}");
        assert_eq!(upstream["max_tokens"], 128_000, "{case_name}");
        assert_no_openai_or_internal_control_keys(&upstream);
    }
}

#[tokio::test]
async fn reasoning_controls_are_not_injected_when_absent() {
    let cases = [
        (
            "native messages",
            "/v1/messages",
            json!({
                "model": "mock,test-model",
                "messages": [{"role": "user", "content": "hello"}],
                "max_tokens": 128_000,
                "stream": false
            }),
        ),
        (
            "chat completions",
            "/v1/chat/completions",
            json!({
                "model": "mock,test-model",
                "messages": [{"role": "user", "content": "hello"}],
                "max_tokens": 128_000,
                "stream": false
            }),
        ),
        (
            "responses with a summary preference only",
            "/v1/responses",
            json!({
                "model": "mock,test-model", "input": "hello",
                "reasoning": {"summary": "auto"},
                "max_output_tokens": 128_000, "stream": false
            }),
        ),
    ];

    for (case_name, endpoint, request) in cases {
        let mock_server = MockServer::start().await;
        mount_anthropic_json_success(&mock_server).await;
        let app = app_for_mock(&mock_server);

        let response = send_json(app, endpoint, request).await;
        let response = successful_json_response(response).await;
        assert_eq!(response["model"], "test-model", "{case_name}");

        let upstream = captured_upstream_json(&mock_server).await;
        assert!(
            upstream.get("thinking").is_none(),
            "{case_name}: thinking was injected"
        );
        assert!(
            upstream.get("output_config").is_none(),
            "{case_name}: output_config was injected"
        );
        assert_no_openai_or_internal_control_keys(&upstream);
    }
}

#[tokio::test]
async fn explicit_output_config_effort_wins_conflicting_reasoning_effort() {
    let mock_server = MockServer::start().await;
    mount_anthropic_json_success(&mock_server).await;
    let app = app_for_mock(&mock_server);
    let native_output_config = json!({
        "effort": "low",
        "budget_source": "explicit",
        "include": ["reasoning"]
    });

    let response = send_json(
        app,
        "/v1/chat/completions",
        json!({
            "model": "mock,test-model",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 128_000,
            "reasoning_effort": "high",
            "output_config": native_output_config,
            "stream": false
        }),
    )
    .await;
    let response = successful_json_response(response).await;
    assert_eq!(response["choices"][0]["message"]["content"], "ok");

    let upstream = captured_upstream_json(&mock_server).await;
    assert_eq!(upstream["output_config"], native_output_config);
    assert_eq!(upstream["max_tokens"], 128_000);
    assert_no_openai_or_internal_control_keys(&upstream);
}

#[tokio::test]
async fn tool_result_normalization_preserves_anthropic_reasoning_controls() {
    let mock_server = MockServer::start().await;
    mount_anthropic_json_success(&mock_server).await;
    let app = app_for_mock(&mock_server);
    let thinking = json!({"type": "enabled", "budget_tokens": 4096});
    let format = json!({"type": "json_schema", "schema": {"type": "object"}});

    let response = send_json(
        app,
        "/v1/chat/completions",
        json!({
            "model": "mock,test-model",
            "messages": [
                {"role": "user", "content": "What time is it in UTC?"},
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_time",
                        "type": "function",
                        "function": {
                            "name": "get_time",
                            "arguments": "{\"timezone\":\"UTC\"}"
                        }
                    }]
                },
                {
                    "role": "tool",
                    "tool_call_id": "call_time",
                    "content": "12:34"
                }
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_time",
                    "description": "Get the current time",
                    "parameters": {
                        "type": "object",
                        "properties": {"timezone": {"type": "string"}}
                    }
                }
            }],
            "max_tokens": 128_000,
            "reasoning_effort": "high",
            "thinking": thinking,
            "output_config": {"format": format},
            "stream": false
        }),
    )
    .await;
    let response = successful_json_response(response).await;
    assert_eq!(response["choices"][0]["message"]["content"], "ok");

    let upstream = captured_upstream_json(&mock_server).await;
    let messages = upstream["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"][0]["type"], "text");
    assert_eq!(messages[1]["role"], "assistant");
    let tool_use = messages[1]["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["type"] == "tool_use")
        .expect("normalized assistant message must contain the tool use");
    assert_eq!(tool_use["id"], "call_time");
    assert_eq!(tool_use["name"], "get_time");
    assert_eq!(tool_use["input"], json!({"timezone": "UTC"}));
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(messages[2]["content"][0]["type"], "tool_result");
    assert_eq!(messages[2]["content"][0]["tool_use_id"], "call_time");
    assert_eq!(messages[2]["content"][0]["content"], "12:34");

    assert_eq!(upstream["tools"][0]["name"], "get_time");
    assert!(upstream["tools"][0].get("function").is_none());
    assert_eq!(upstream["max_tokens"], 128_000);
    assert_eq!(upstream["thinking"], thinking);
    assert_eq!(
        upstream["output_config"],
        json!({"effort": "high", "format": format})
    );
    assert_no_openai_or_internal_control_keys(&upstream);
}

#[tokio::test]
async fn chat_reasoning_effort_maps_and_survives_anthropic_streaming() {
    let mock_server = MockServer::start().await;
    mount_anthropic_sse_success(&mock_server).await;
    let app = app_for_mock(&mock_server);

    let response = send_json(
        app,
        "/v1/chat/completions",
        json!({
            "model": "mock,test-model",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 128_000,
            "reasoning_effort": "high",
            "stream": true
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("streamed ok"), "stream body: {body}");
    assert!(body.contains("data: [DONE]"), "stream body: {body}");

    let upstream = captured_upstream_json(&mock_server).await;
    assert_eq!(upstream["stream"], true);
    assert_eq!(upstream["max_tokens"], 128_000);
    assert_eq!(upstream["output_config"], json!({"effort": "high"}));
    assert_no_openai_or_internal_control_keys(&upstream);
}

#[tokio::test]
async fn native_effort_reaches_openai_upstream() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl_effort", "object": "chat.completion", "created": 0, "model": "test-model",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 4, "completion_tokens": 2, "total_tokens": 6}
        })))
        .expect(1)
        .mount(&mock_server)
        .await;
    let mut config: Value =
        serde_json::from_str(&make_anthropic_config(&mock_server.uri())).unwrap();
    config["Providers"][0]["protocol"] = json!("openai");
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.json");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let app =
        build_app(ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap());
    let response = send_json(
        app,
        "/v1/messages",
        json!({
            "model": "mock,test-model", "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 128_000, "stream": false, "output_config": {"effort": "max"}
        }),
    )
    .await;
    let response = successful_json_response(response).await;
    assert_eq!(response["content"][0]["text"], "ok");
    let requests = mock_server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let upstream: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(upstream["reasoning_effort"], "max");
    assert_eq!(upstream["max_tokens"], 128_000);
    assert!(upstream.get("output_config").is_none());
}
