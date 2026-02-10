//! Integration tests for Codex streaming conversion from Anthropic SSE to OpenAI SSE.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use bytes::Bytes;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::time::{timeout, Duration};
use tokio_stream::wrappers::ReceiverStream;
use tower::ServiceExt;

/// Skip integration tests that require opening localhost sockets when the
/// execution environment forbids binding ports.
fn skip_if_localhost_bind_unavailable() -> bool {
    if std::net::TcpListener::bind("127.0.0.1:0").is_ok() {
        return false;
    }

    eprintln!("Skipping test: cannot bind localhost sockets in this environment");
    true
}

fn make_anthropic_test_config(base_url: &str) -> String {
    let config = json!({
        "Providers": [
            {
                "name": "mock",
                "api_base_url": base_url,
                "api_key": "test-key",
                "models": ["test-model"],
                "protocol": "anthropic"
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

fn codex_stream_request_body() -> Value {
    json!({
        "model": "mock,test-model",
        "messages": [
            {"role": "user", "content": "Stream please"}
        ],
        "max_tokens": 100,
        "stream": true
    })
}

fn sse_event(event_type: &str, payload: Value) -> String {
    format!("event: {event_type}\ndata: {payload}\n\n")
}

fn parse_sse_data_frames(payload: &str) -> Vec<String> {
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
                    data_lines.push(rest.trim_start().to_string());
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

async fn start_anthropic_stream_server(chunks: Vec<(Bytes, u64)>) -> String {
    let chunks = std::sync::Arc::new(chunks);
    let app = Router::new().route(
        "/messages",
        post({
            let chunks = chunks.clone();
            move || {
                let chunks = chunks.clone();
                async move {
                    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
                    tokio::spawn(async move {
                        for (chunk, delay_ms) in chunks.iter() {
                            if tx.send(Ok(chunk.clone())).await.is_err() {
                                return;
                            }
                            if *delay_ms > 0 {
                                tokio::time::sleep(Duration::from_millis(*delay_ms)).await;
                            }
                        }
                    });

                    Response::builder()
                        .status(StatusCode::OK)
                        .header("content-type", "text/event-stream")
                        .body(Body::from_stream(ReceiverStream::new(rx)))
                        .unwrap()
                }
            }
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    format!("http://{}", addr)
}

async fn make_codex_stream_request(app: &Router) -> Response {
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

#[tokio::test]
async fn test_anthropic_stream_chunk_boundary_inside_frame_is_parsed() {
    // Skip if we cannot bind localhost sockets in this environment
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let message_start = sse_event(
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": "msg_boundary",
                "type": "message",
                "role": "assistant",
                "model": "claude-3-opus",
                "content": []
            }
        }),
    );
    let split_at = message_start
        .find("\"message\"")
        .expect("message_start should contain message key")
        + 6;
    let (part_a, part_b) = message_start.split_at(split_at);

    let stream_chunks = vec![
        (Bytes::from(part_a.to_string()), 0),
        (Bytes::from(part_b.to_string()), 0),
        (
            Bytes::from(sse_event(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {
                        "type": "text_delta",
                        "text": "split-boundary-ok"
                    }
                }),
            )),
            0,
        ),
        (
            Bytes::from(sse_event(
                "message_delta",
                json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn", "stop_sequence": null}
                }),
            )),
            0,
        ),
        (
            Bytes::from(sse_event("message_stop", json!({"type": "message_stop"}))),
            0,
        ),
    ];

    let upstream_url = start_anthropic_stream_server(stream_chunks).await;
    let config_json = make_anthropic_test_config(&upstream_url);
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();

    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let resp = make_codex_stream_request(&app).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "text/event-stream"
    );

    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body_text = String::from_utf8(body.to_vec()).unwrap();
    let data_frames = parse_sse_data_frames(&body_text);
    let json_events: Vec<Value> = data_frames
        .iter()
        .filter(|frame| frame.as_str() != "[DONE]")
        .map(|frame| serde_json::from_str(frame).unwrap())
        .collect();

    assert!(json_events
        .iter()
        .any(|evt| evt["choices"][0]["delta"]["role"].as_str() == Some("assistant")));
    assert!(json_events
        .iter()
        .any(|evt| evt["choices"][0]["delta"]["content"].as_str() == Some("split-boundary-ok")));
}

#[tokio::test]
async fn test_anthropic_stream_emits_first_assistant_delta_before_completion() {
    // Skip if we cannot bind localhost sockets in this environment
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let stream_chunks = vec![
        (
            Bytes::from(sse_event(
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": "msg_early",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-3-opus",
                        "content": []
                    }
                }),
            )),
            1200,
        ),
        (
            Bytes::from(sse_event(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "text_delta", "text": "late tail"}
                }),
            )),
            0,
        ),
        (
            Bytes::from(sse_event("message_stop", json!({"type": "message_stop"}))),
            0,
        ),
    ];

    let upstream_url = start_anthropic_stream_server(stream_chunks).await;
    let config_json = make_anthropic_test_config(&upstream_url);
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();

    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let resp = timeout(Duration::from_millis(700), make_codex_stream_request(&app))
        .await
        .expect("response should start before upstream stream completes");
    assert_eq!(resp.status(), StatusCode::OK);

    let mut stream = resp.into_body().into_data_stream();
    let first_chunk = timeout(Duration::from_millis(700), stream.next())
        .await
        .expect("first chunk should arrive before stream completion")
        .expect("stream should contain first chunk")
        .expect("first chunk should be readable");
    let first_text = String::from_utf8(first_chunk.to_vec()).unwrap();

    assert!(first_text.contains("\"role\":\"assistant\""));
    assert!(!first_text.contains("[DONE]"));

    let mut rest = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        rest.push_str(std::str::from_utf8(&chunk).unwrap());
    }
    let full_stream = format!("{first_text}{rest}");

    let assistant_index = full_stream
        .find("\"role\":\"assistant\"")
        .expect("assistant delta should be present");
    let done_index = full_stream
        .rfind("data: [DONE]")
        .expect("done marker should be present");
    assert!(assistant_index < done_index);
}

#[tokio::test]
async fn test_anthropic_stream_tool_deltas_and_stop_events_are_well_formed() {
    // Skip if we cannot bind localhost sockets in this environment
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let stream_chunks = vec![
        (
            Bytes::from(sse_event(
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": "msg_tool",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-3-opus",
                        "content": []
                    }
                }),
            )),
            0,
        ),
        (
            Bytes::from(sse_event(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {
                        "type": "tool_use",
                        "id": "toolu_calc_1",
                        "name": "calculator",
                        "input": {}
                    }
                }),
            )),
            0,
        ),
        (
            Bytes::from(sse_event(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": "{\"expression\":\"2+2\"}"
                    }
                }),
            )),
            0,
        ),
        (
            Bytes::from(sse_event(
                "message_delta",
                json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "tool_use", "stop_sequence": null}
                }),
            )),
            0,
        ),
        (
            Bytes::from(sse_event("message_stop", json!({"type": "message_stop"}))),
            0,
        ),
    ];

    let upstream_url = start_anthropic_stream_server(stream_chunks).await;
    let config_json = make_anthropic_test_config(&upstream_url);
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();

    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let resp = make_codex_stream_request(&app).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body_text = String::from_utf8(body.to_vec()).unwrap();
    let data_frames = parse_sse_data_frames(&body_text);
    let json_events: Vec<Value> = data_frames
        .iter()
        .filter(|frame| frame.as_str() != "[DONE]")
        .map(|frame| serde_json::from_str(frame).unwrap())
        .collect();

    let tool_start_event = json_events
        .iter()
        .find(|evt| evt["choices"][0]["delta"]["tool_calls"].is_array())
        .expect("tool start delta should be present");
    assert_eq!(
        tool_start_event["choices"][0]["delta"]["tool_calls"][0]["id"],
        "toolu_calc_1"
    );
    assert_eq!(
        tool_start_event["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
        "calculator"
    );

    let tool_delta_event = json_events
        .iter()
        .find(|evt| {
            evt["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
        })
        .expect("tool argument delta should be present");
    assert_eq!(
        tool_delta_event["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
        "{\"expression\":\"2+2\"}"
    );

    let stop_event = json_events
        .iter()
        .find(|evt| evt["choices"][0]["finish_reason"].as_str() == Some("tool_calls"))
        .expect("tool stop event should be present");
    assert_eq!(stop_event["choices"][0]["delta"], json!({}));
}

#[tokio::test]
async fn test_anthropic_stream_emits_exactly_one_done_marker_at_end() {
    // Skip if we cannot bind localhost sockets in this environment
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let stream_chunks = vec![
        (
            Bytes::from(sse_event(
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": "msg_done",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-3-opus",
                        "content": []
                    }
                }),
            )),
            0,
        ),
        (
            Bytes::from(sse_event(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "text_delta", "text": "hello"}
                }),
            )),
            0,
        ),
        (
            Bytes::from(sse_event("message_stop", json!({"type": "message_stop"}))),
            0,
        ),
        (Bytes::from("data: [DONE]\n\n".to_string()), 0),
    ];

    let upstream_url = start_anthropic_stream_server(stream_chunks).await;
    let config_json = make_anthropic_test_config(&upstream_url);
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();

    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let resp = make_codex_stream_request(&app).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body_text = String::from_utf8(body.to_vec()).unwrap();
    let data_frames = parse_sse_data_frames(&body_text);
    let done_count = data_frames
        .iter()
        .filter(|frame| frame.as_str() == "[DONE]")
        .count();

    assert_eq!(done_count, 1);
    assert_eq!(data_frames.last().map(String::as_str), Some("[DONE]"));
}

#[tokio::test]
async fn test_anthropic_stream_utf8_split_across_chunks() {
    // Skip if we cannot bind localhost sockets in this environment
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    // "hello" in Japanese is "こんにちは" (Kon'nichiwa)
    // 'こ' is E3 81 93
    let text = "こ";
    let bytes = text.as_bytes();
    assert_eq!(bytes.len(), 3);

    let part_a = vec![bytes[0]];
    let part_b = vec![bytes[1], bytes[2]];

    let frame_start = sse_event(
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": "msg_utf8",
                "type": "message",
                "role": "assistant",
                "model": "claude-3-opus",
                "content": []
            }
        }),
    );

    let delta_prefix = "event: content_block_delta\ndata: {\"type\": \"content_block_delta\", \"index\": 0, \"delta\": {\"type\": \"text_delta\", \"text\": \"";
    let delta_suffix = "\"}}\n\n";

    let stream_chunks = vec![
        (Bytes::from(frame_start), 0),
        (Bytes::from(delta_prefix.to_string()), 0),
        (Bytes::from(part_a), 0),
        (Bytes::from(part_b), 0),
        (Bytes::from(delta_suffix.to_string()), 0),
        (
            Bytes::from(sse_event("message_stop", json!({"type": "message_stop"}))),
            0,
        ),
    ];

    let upstream_url = start_anthropic_stream_server(stream_chunks).await;
    let config_json = make_anthropic_test_config(&upstream_url);
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();

    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let resp = make_codex_stream_request(&app).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body_text = String::from_utf8(body.to_vec()).unwrap();
    let data_frames = parse_sse_data_frames(&body_text);
    let json_events: Vec<Value> = data_frames
        .iter()
        .filter(|frame| frame.as_str() != "[DONE]")
        .map(|frame| serde_json::from_str(frame).unwrap())
        .collect();

    let utf8_delta = json_events
        .iter()
        .find(|evt| evt["choices"][0]["delta"]["content"].as_str() == Some("こ"))
        .expect("UTF-8 character should be correctly reassembled and present in the output");

    assert_eq!(utf8_delta["choices"][0]["delta"]["content"], "こ");
}

#[tokio::test]
async fn test_anthropic_stream_abrupt_closure_emits_done_marker() {
    // Skip if we cannot bind localhost sockets in this environment
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let stream_chunks = vec![
        (
            Bytes::from(sse_event(
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": "msg_abrupt",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-3-opus",
                        "content": []
                    }
                }),
            )),
            0,
        ),
        (
            Bytes::from(sse_event(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "text_delta", "text": "abrupt"}
                }),
            )),
            0,
        ),
        // No message_stop or [DONE] here
    ];

    let upstream_url = start_anthropic_stream_server(stream_chunks).await;
    let config_json = make_anthropic_test_config(&upstream_url);
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();

    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let resp = make_codex_stream_request(&app).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body_text = String::from_utf8(body.to_vec()).unwrap();
    let data_frames = parse_sse_data_frames(&body_text);

    assert!(data_frames.iter().any(|frame| frame.contains("abrupt")));
    assert_eq!(data_frames.last().map(String::as_str), Some("[DONE]"));
}
