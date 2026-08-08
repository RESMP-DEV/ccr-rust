// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration tests for cached-input token accounting.
//!
//! Providers report cached prompt activity in protocol-specific shapes:
//! Anthropic-style responses carry `cache_read_input_tokens` and
//! `cache_creation_input_tokens` next to an `input_tokens` value that excludes
//! them, while OpenAI-style responses fold the cached share into
//! `prompt_tokens` and break it out under `prompt_tokens_details.cached_tokens`.
//! These tests prove that both shapes land normalized in `/v1/usage` (full
//! prompt volume plus cache splits) and that configured cached rates discount
//! the cost estimate.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::Router;
use serde_json::json;
use std::io::Write;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Write test config to temp file and return the handle.
fn write_config_file(config_json: &serde_json::Value) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    serde_json::to_writer_pretty(&mut f, config_json).unwrap();
    f.flush().unwrap();
    f
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

/// Skip integration tests that require opening localhost sockets.
fn skip_if_localhost_bind_unavailable(test_name: &str) -> bool {
    if std::net::TcpListener::bind("127.0.0.1:0").is_ok() {
        return false;
    }
    eprintln!("Skipping {test_name}: cannot bind localhost sockets");
    true
}

/// Fetch the /v1/usage summary through the real handler.
async fn fetch_usage_summary() -> serde_json::Value {
    let resp =
        axum::response::IntoResponse::into_response(ccr_rust::metrics::usage_handler().await);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Find the tier entry whose name contains `needle`.
fn find_tier(summary: &serde_json::Value, needle: &str) -> Option<serde_json::Value> {
    summary["tiers"]
        .as_array()?
        .iter()
        .find(|t| t["tier"].as_str().is_some_and(|name| name.contains(needle)))
        .cloned()
}

/// Poll /v1/usage until the tier shows recorded input tokens. Streaming
/// paths record usage from a spawned task after the last frame is forwarded,
/// so the metrics can land slightly after the response body completes.
async fn wait_for_tier_usage(needle: &str) -> serde_json::Value {
    for _ in 0..100 {
        let summary = fetch_usage_summary().await;
        if let Some(tier) = find_tier(&summary, needle) {
            if tier["input_tokens"].as_u64().unwrap_or(0) > 0 {
                return summary;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("tier {needle} never showed recorded usage in /v1/usage");
}

fn approx_eq(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 1e-9
}

#[tokio::test]
async fn anthropic_nonstream_cache_fields_recorded_and_priced() {
    if skip_if_localhost_bind_unavailable("anthropic_nonstream_cache_fields_recorded_and_priced") {
        return;
    }
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_cached_1",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Hello"}],
            "model": "claude-sonnet-4-6",
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 3,
                "output_tokens": 42,
                "cache_read_input_tokens": 5000,
                "cache_creation_input_tokens": 100
            }
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let config_json = json!({
        "Providers": [
            {
                "name": "cache-nonstream-probe",
                "api_base_url": format!("{}/v1", mock_server.uri()),
                "api_key": "test-key",
                "models": ["claude-sonnet-4-6"],
                "protocol": "anthropic",
                "pricing": {
                    "input_per_million_tokens": 1.0,
                    "output_per_million_tokens": 2.0,
                    "cache_read_per_million_tokens": 0.1,
                    "cache_creation_per_million_tokens": 1.25
                }
            }
        ],
        "Router": {
            "default": "cache-nonstream-probe,claude-sonnet-4-6"
        },
        "API_TIMEOUT_MS": 5000
    });
    let config_file = write_config_file(&config_json);
    let config = ccr_rust::config::Config::from_file(config_file.path().to_str().unwrap()).unwrap();
    let app = build_app(config);

    let request_body = json!({
        "model": "cache-nonstream-probe,claude-sonnet-4-6",
        "messages": [{"role": "user", "content": "Say hello"}],
        "max_tokens": 100
    });
    let response = app
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
    assert_eq!(response.status(), StatusCode::OK);

    // The client-visible passthrough keeps both cache fields.
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(resp["usage"]["cache_read_input_tokens"], 5000);
    assert_eq!(resp["usage"]["cache_creation_input_tokens"], 100);

    // Non-streaming recording is synchronous with the response.
    let summary = fetch_usage_summary().await;
    let tier = find_tier(&summary, "cache-nonstream-probe").expect("tier recorded");
    // input_tokens carries the full prompt volume: 3 uncached + 5000 read + 100 created.
    assert_eq!(tier["input_tokens"], 5103);
    assert_eq!(tier["output_tokens"], 42);
    assert_eq!(tier["cache_read_tokens"], 5000);
    assert_eq!(tier["cache_creation_tokens"], 100);
    // Cost bills each slice at its own rate:
    // (3 * 1.0 + 42 * 2.0 + 5000 * 0.1 + 100 * 1.25) / 1e6 = 712e-6.
    let cost = tier["cost_usd"].as_f64().unwrap();
    assert!(
        approx_eq(cost, 712e-6),
        "expected cached-aware cost 0.000712, got {cost}"
    );

    // Top-level totals include this tier's cache splits (other tests in this
    // binary may add more, so lower-bound assertions only).
    assert!(summary["total_cache_read_tokens"].as_u64().unwrap() >= 5000);
    assert!(summary["total_cache_creation_tokens"].as_u64().unwrap() >= 100);
    assert!(summary["total_input_tokens"].as_u64().unwrap() >= 5103);
}

#[tokio::test]
async fn anthropic_stream_message_start_cache_fields_recorded() {
    if skip_if_localhost_bind_unavailable("anthropic_stream_message_start_cache_fields_recorded") {
        return;
    }
    let mock_server = MockServer::start().await;

    // message_start reports input and cache splits under message.usage;
    // message_delta reports cumulative output under a top-level usage.
    let sse_body = format!(
        "event: message_start\ndata: {}\n\n\
         event: content_block_start\ndata: {}\n\n\
         event: content_block_delta\ndata: {}\n\n\
         event: content_block_stop\ndata: {}\n\n\
         event: message_delta\ndata: {}\n\n\
         event: message_stop\ndata: {}\n\n",
        json!({
            "type": "message_start",
            "message": {
                "id": "msg_cached_stream",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-6",
                "content": [],
                "usage": {
                    "input_tokens": 3,
                    "output_tokens": 1,
                    "cache_read_input_tokens": 5000,
                    "cache_creation_input_tokens": 100
                }
            }
        }),
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "Hello there"}
        }),
        json!({"type": "content_block_stop", "index": 0}),
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 42}
        }),
        json!({"type": "message_stop"})
    );

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body)
                .insert_header("content-type", "text/event-stream"),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let config_json = json!({
        "Providers": [
            {
                "name": "cache-stream-probe",
                "api_base_url": format!("{}/v1", mock_server.uri()),
                "api_key": "test-key",
                "models": ["claude-sonnet-4-6"],
                "protocol": "anthropic",
                "pricing": {
                    "input_per_million_tokens": 1.0,
                    "output_per_million_tokens": 2.0,
                    "cache_read_per_million_tokens": 0.1,
                    "cache_creation_per_million_tokens": 1.25
                }
            }
        ],
        "Router": {
            "default": "cache-stream-probe,claude-sonnet-4-6"
        },
        "API_TIMEOUT_MS": 5000
    });
    let config_file = write_config_file(&config_json);
    let config = ccr_rust::config::Config::from_file(config_file.path().to_str().unwrap()).unwrap();
    let app = build_app(config);

    let request_body = json!({
        "model": "cache-stream-probe,claude-sonnet-4-6",
        "messages": [{"role": "user", "content": "Say hello"}],
        "max_tokens": 100,
        "stream": true
    });
    let response = app
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
    assert_eq!(response.status(), StatusCode::OK);
    // Drain the passthrough stream to completion.
    let _ = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let summary = wait_for_tier_usage("cache-stream-probe").await;
    let tier = find_tier(&summary, "cache-stream-probe").expect("tier recorded");
    assert_eq!(tier["input_tokens"], 5103);
    assert_eq!(tier["output_tokens"], 42);
    assert_eq!(tier["cache_read_tokens"], 5000);
    assert_eq!(tier["cache_creation_tokens"], 100);
    let cost = tier["cost_usd"].as_f64().unwrap();
    assert!(
        approx_eq(cost, 712e-6),
        "expected cached-aware cost 0.000712, got {cost}"
    );
}

#[tokio::test]
async fn openai_cached_tokens_priced_at_cached_rate() {
    if skip_if_localhost_bind_unavailable("openai_cached_tokens_priced_at_cached_rate") {
        return;
    }
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-cached",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "claude-sonnet-4-6",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hello"},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 100,
                "total_tokens": 1100,
                "prompt_tokens_details": {"cached_tokens": 800}
            }
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    // camelCase aliases exercise the serde alias path for cached rates.
    let config_json = json!({
        "Providers": [
            {
                "name": "cache-openai-probe",
                "api_base_url": mock_server.uri(),
                "api_key": "test-key",
                "models": ["claude-sonnet-4-6"],
                "pricing": {
                    "inputPerMillionTokens": 3.0,
                    "outputPerMillionTokens": 15.0,
                    "cacheReadPerMillionTokens": 0.3
                }
            }
        ],
        "Router": {
            "default": "cache-openai-probe,claude-sonnet-4-6"
        },
        "API_TIMEOUT_MS": 5000
    });
    let config_file = write_config_file(&config_json);
    let config = ccr_rust::config::Config::from_file(config_file.path().to_str().unwrap()).unwrap();
    let app = build_app(config);

    let request_body = json!({
        "model": "cache-openai-probe,claude-sonnet-4-6",
        "messages": [{"role": "user", "content": "Say hello"}],
        "max_tokens": 100
    });
    let response = app
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
    assert_eq!(response.status(), StatusCode::OK);

    let summary = fetch_usage_summary().await;
    let tier = find_tier(&summary, "cache-openai-probe").expect("tier recorded");
    // OpenAI prompt_tokens already include the cached share.
    assert_eq!(tier["input_tokens"], 1000);
    assert_eq!(tier["output_tokens"], 100);
    assert_eq!(tier["cache_read_tokens"], 800);
    assert_eq!(tier["cache_creation_tokens"], 0);
    // (200 * 3.0 + 800 * 0.3 + 100 * 15.0) / 1e6 = 2340e-6.
    let cost = tier["cost_usd"].as_f64().unwrap();
    assert!(
        approx_eq(cost, 2340e-6),
        "expected cached-aware cost 0.00234, got {cost}"
    );
}

async fn run_openai_stream_usage_case(
    tier_name: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_tokens: u64,
) -> serde_json::Value {
    let mock_server = MockServer::start().await;
    let sse_body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({
            "id": "chatcmpl-stream-cached",
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
            "id": "chatcmpl-stream-cached",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": "claude-sonnet-4-6",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": prompt_tokens,
                "completion_tokens": completion_tokens,
                "total_tokens": prompt_tokens.saturating_add(completion_tokens),
                "prompt_tokens_details": {"cached_tokens": cached_tokens}
            }
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

    let config_json = json!({
        "Providers": [{
            "name": tier_name,
            "api_base_url": mock_server.uri(),
            "api_key": "test-key",
            "models": ["claude-sonnet-4-6"],
            "pricing": {
                "input_per_million_tokens": 3.0,
                "output_per_million_tokens": 15.0,
                "cache_read_per_million_tokens": 0.3
            }
        }],
        "Router": {"default": format!("{tier_name},claude-sonnet-4-6")},
        "API_TIMEOUT_MS": 5000
    });
    let config_file = write_config_file(&config_json);
    let config = ccr_rust::config::Config::from_file(config_file.path().to_str().unwrap()).unwrap();
    let app = build_app(config);
    let message_content = if prompt_tokens == 0 && cached_tokens > 0 {
        "alpha beta gamma delta ".repeat(1000)
    } else {
        "Say hello from the streaming test".to_string()
    };
    let request_body = json!({
        "model": format!("{tier_name},claude-sonnet-4-6"),
        "messages": [{"role": "user", "content": message_content}],
        "max_tokens": 100,
        "stream": true
    });
    let response = app
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
    assert_eq!(response.status(), StatusCode::OK);
    let _ = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    // Draining the body waits for the stream task to record usage and drop its
    // sole sender, so the metrics snapshot is complete here.
    let summary = fetch_usage_summary().await;
    find_tier(&summary, tier_name).expect("streaming tier recorded")
}

#[tokio::test]
async fn openai_stream_cached_tokens_are_recorded_and_discounted() {
    if skip_if_localhost_bind_unavailable("openai_stream_cached_tokens_are_recorded_and_discounted")
    {
        return;
    }
    let tier = run_openai_stream_usage_case("cache-openai-stream-probe", 1000, 100, 800).await;
    assert_eq!(tier["input_tokens"], 1000);
    assert_eq!(tier["output_tokens"], 100);
    assert_eq!(tier["cache_read_tokens"], 800);
    let cost = tier["cost_usd"].as_f64().unwrap();
    assert!(approx_eq(cost, 2340e-6), "got {cost}");
}

#[tokio::test]
async fn openai_stream_zero_prompt_tokens_use_the_local_estimate() {
    if skip_if_localhost_bind_unavailable("openai_stream_zero_prompt_tokens_use_the_local_estimate")
    {
        return;
    }
    let tier_name = "cache-openai-stream-fallback";
    let tier = run_openai_stream_usage_case(tier_name, 0, 5, 800).await;
    let input_tokens = tier["input_tokens"].as_u64().unwrap_or(0);
    assert!(input_tokens > 800);
    assert_eq!(tier["output_tokens"], 5);
    assert_eq!(tier["cache_read_tokens"], 800);
    let expected_cost =
        ((input_tokens - 800) as f64 * 3.0 + 800.0 * 0.3 + 5.0 * 15.0) / 1_000_000.0;
    let cost = tier["cost_usd"].as_f64().unwrap();
    assert!(approx_eq(cost, expected_cost), "got {cost}");

    let drift_response =
        axum::response::IntoResponse::into_response(ccr_rust::metrics::token_drift_handler().await);
    let drift_bytes = axum::body::to_bytes(drift_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let drift: serde_json::Value = serde_json::from_slice(&drift_bytes).unwrap();
    assert!(
        drift
            .as_array()
            .is_some_and(|entries| entries.iter().all(|entry| entry["tier"] != tier_name)),
        "zero upstream prompt usage must not create a false drift sample"
    );
}

#[test]
fn usage_summary_deserializes_legacy_payload_without_cache_totals() {
    // A dashboard binary newer than the running service must tolerate a
    // summary that predates the cached-token totals.
    let legacy = r#"{
        "total_requests": 5,
        "total_failures": 1,
        "total_input_tokens": 100,
        "total_output_tokens": 50,
        "total_cost_usd": 0.0,
        "active_streams": 0.0,
        "active_requests": 0.0,
        "tiers": []
    }"#;
    let summary: ccr_rust::metrics::UsageSummary = serde_json::from_str(legacy).unwrap();
    assert_eq!(summary.total_cache_read_tokens, 0);
    assert_eq!(summary.total_cache_creation_tokens, 0);
}
