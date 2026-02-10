//! Integration tests for Codex streaming conversion when upstream is OpenAI-compatible SSE.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use bytes::Bytes;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::time::Duration;
use tokio_stream::wrappers::ReceiverStream;
use tower::ServiceExt;

fn make_openai_test_config(base_url: &str) -> String {
    let config = json!({
        "Providers": [
            {
                "name": "mock",
                "api_base_url": base_url,
                "api_key": "test-key",
                "models": ["test-model"],
                "protocol": "openai"
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

/// Skip integration tests that require opening localhost sockets when the
/// execution environment forbids binding ports.
fn skip_if_localhost_bind_unavailable() -> bool {
    if std::net::TcpListener::bind("127.0.0.1:0").is_ok() {
        return false;
    }

    eprintln!("Skipping test: cannot bind localhost sockets in this environment");
    true
}

async fn start_openai_stream_server(chunks: Vec<(Bytes, u64)>) -> String {
    let chunks = std::sync::Arc::new(chunks);
    let app = Router::new().route(
        "/chat/completions",
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
async fn test_codex_stream_reassembles_fragmented_openai_sse_frames() {
    // Skip if we cannot bind localhost sockets in this environment
    if skip_if_localhost_bind_unavailable() {
        return;
    }
    let role_frame = format!(
        "data: {}\n\n",
        json!({
            "id": "chunk_1",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant"},
                "finish_reason": null
            }]
        })
    );

    let content_frame = format!(
        "data: {}\n\n",
        json!({
            "id": "chunk_2",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {"content": "split-boundary-ok"},
                "finish_reason": null
            }]
        })
    );

    let stop_frame = format!(
        "data: {}\n\n",
        json!({
            "id": "chunk_3",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }]
        })
    );

    let role_split = role_frame
        .find("\"assistant\"")
        .expect("role frame should contain assistant")
        + 4;
    let content_split = content_frame
        .find("split-boundary-ok")
        .expect("content frame should contain marker")
        + 5;
    let (role_a, role_b) = role_frame.split_at(role_split);
    let (content_a, content_b) = content_frame.split_at(content_split);

    let stream_chunks = vec![
        (Bytes::from(role_a.to_string()), 0),
        (Bytes::from(role_b.to_string()), 0),
        (Bytes::from(content_a.to_string()), 0),
        (Bytes::from(content_b.to_string()), 0),
        (Bytes::from(stop_frame), 0),
        (Bytes::from("data: [DONE]\n\n"), 0),
    ];

    let upstream_url = start_openai_stream_server(stream_chunks).await;
    let config_json = make_openai_test_config(&upstream_url);
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

    let done_count = data_frames
        .iter()
        .filter(|frame| frame.as_str() == "[DONE]")
        .count();
    assert_eq!(done_count, 1);
    assert_eq!(data_frames.last().map(String::as_str), Some("[DONE]"));

    let json_events: Vec<Value> = data_frames
        .iter()
        .filter(|frame| frame.as_str() != "[DONE]")
        .map(|frame| serde_json::from_str(frame).unwrap())
        .collect();

    assert!(
        json_events
            .iter()
            .any(|evt| evt["choices"][0]["delta"]["role"].as_str() == Some("assistant")),
        "assistant role delta should be present"
    );
    assert!(
        json_events
            .iter()
            .any(|evt| evt["choices"][0]["delta"]["content"].as_str() == Some("split-boundary-ok")),
        "split content delta should be present"
    );
}
