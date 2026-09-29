// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration tests for router-level retry sweeps (`Router.retrySweeps`).
//!
//! With sweeps enabled, a cascade that exhausts every tier must keep the
//! client request in flight and re-cascade after a cooldown instead of
//! surfacing a synthesized 429/503 that trips the client harness's own
//! give-up path (Claude Code, Codex, ...). These tests drive the full
//! `/v1/messages` handler against a stateful local upstream that fails a
//! controlled number of times before recovering.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Skip integration tests that require opening localhost sockets when the
/// execution environment forbids binding ports.
fn skip_if_localhost_bind_unavailable(test_name: &str) -> bool {
    if std::net::TcpListener::bind("127.0.0.1:0").is_ok() {
        return false;
    }

    eprintln!("Skipping {test_name}: cannot bind localhost sockets in this environment");
    true
}

/// Stateful upstream: returns `failure_status` for the first `failures`
/// hits, then a valid OpenAI chat completion. `failures = usize::MAX` never
/// recovers.
async fn spawn_flaky_upstream(failures: usize, failure_status: u16) -> (String, Arc<AtomicUsize>) {
    let failure_status = StatusCode::from_u16(failure_status).expect("valid status code");
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let app = Router::new().route(
        "/chat/completions",
        post(move || {
            let hits = counter.clone();
            async move {
                let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
                if n <= failures {
                    return (failure_status, "upstream exploded").into_response();
                }
                axum::Json(json!({
                    "id": format!("resp_{n}"),
                    "object": "chat.completion",
                    "created": 1234567890,
                    "model": "test-model",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "recovered"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1}
                }))
                .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), hits)
}

/// Build test config pointing at the flaky upstream. `retry_sweeps` is the
/// raw `Router.retrySweeps` JSON value.
fn make_test_config(upstream_url: &str, retry_sweeps: Value) -> String {
    let config = json!({
        "Providers": [
            {
                "name": "mock",
                "api_base_url": upstream_url,
                "api_key": "test-key",
                "models": ["test-model"]
            }
        ],
        "Router": {
            "default": "mock,test-model",
            // Zero in-tier retries so hit counts map 1:1 to sweeps.
            "tierRetries": {"mock": {"max_retries": 0}},
            "retrySweeps": retry_sweeps
        },
        "API_TIMEOUT_MS": 5000
    });
    serde_json::to_string_pretty(&config).unwrap()
}

/// Build the Axum app with test state.
fn build_app(config: ccr_rust::config::Config) -> Router {
    let ewma_tracker = Arc::new(ccr_rust::routing::EwmaTracker::new());
    let transformer_registry = Arc::new(ccr_rust::transformer::TransformerRegistry::new());
    let active_streams = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ratelimit_tracker = Arc::new(ccr_rust::ratelimit::RateLimitTracker::new());
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

fn load_config(config_json: &str) -> ccr_rust::config::Config {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, config_json).unwrap();
    ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap()
}

async fn send_message(app: Router) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "model": "test-model",
                        "messages": [{"role": "user", "content": "hello"}],
                        "max_tokens": 32
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body_bytes).unwrap())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Control: with retrySweeps disabled, one exhausted cascade immediately
/// surfaces the 503 (legacy fail-fast behavior is preserved).
#[tokio::test]
async fn sweeps_disabled_surfaces_503_after_single_cascade() {
    if skip_if_localhost_bind_unavailable("sweeps_disabled_surfaces_503_after_single_cascade") {
        return;
    }
    let (url, hits) = spawn_flaky_upstream(usize::MAX, 500).await;
    let config = make_test_config(&url, json!({"enabled": false}));

    let (status, body) = send_message(build_app(load_config(&config))).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "service_unavailable");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

/// The core sturdiness property: with unlimited sweeps, a request whose
/// upstream fails twice is held open across sweep boundaries and completes
/// successfully once the upstream recovers. The harness never sees a 5xx.
#[tokio::test]
async fn sweeps_hold_request_until_upstream_recovers() {
    if skip_if_localhost_bind_unavailable("sweeps_hold_request_until_upstream_recovers") {
        return;
    }
    let (url, hits) = spawn_flaky_upstream(2, 500).await;
    let config = make_test_config(&url, json!({"enabled": true, "sweepCooldownMs": 25}));

    let (status, body) = send_message(build_app(load_config(&config))).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["type"], "message");
    assert_eq!(body["content"][0]["text"], "recovered");
    assert_eq!(hits.load(Ordering::SeqCst), 3);
}

/// maxSweeps bounds the hold: with two extra sweeps the upstream is hit
/// exactly three times (one per sweep) before the 503 is surfaced, and the
/// error message reports accurate sweep accounting.
#[tokio::test]
async fn sweeps_give_up_after_max_sweeps() {
    if skip_if_localhost_bind_unavailable("sweeps_give_up_after_max_sweeps") {
        return;
    }
    let (url, hits) = spawn_flaky_upstream(usize::MAX, 500).await;
    let config = make_test_config(
        &url,
        json!({"enabled": true, "maxSweeps": 2, "sweepCooldownMs": 25}),
    );

    let (status, body) = send_message(build_app(load_config(&config))).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("across 3 sweep(s)"),
        "expected sweep accounting in error message, got: {message}"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 3);
}

/// Deterministic rejections (401/402/403/404) must never be swept: no
/// amount of retrying fixes a bad credential, so even unlimited sweeps
/// surface the failure after a single cascade.
#[tokio::test]
async fn deterministic_rejections_do_not_sweep() {
    if skip_if_localhost_bind_unavailable("deterministic_rejections_do_not_sweep") {
        return;
    }
    let (url, hits) = spawn_flaky_upstream(usize::MAX, 401).await;
    let config = make_test_config(&url, json!({"enabled": true, "sweepCooldownMs": 25}));

    let (status, _body) = send_message(build_app(load_config(&config))).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

/// Rate-limited tiers keep the request held (sweeps continue while the
/// tracker's backoff window skips the tier) and the final response is the
/// synthesized rate-limit 429, not a generic 5xx.
#[tokio::test]
async fn rate_limited_tier_holds_then_synthesizes_429() {
    if skip_if_localhost_bind_unavailable("rate_limited_tier_holds_then_synthesizes_429") {
        return;
    }
    let (url, hits) = spawn_flaky_upstream(usize::MAX, 429).await;
    let config = make_test_config(
        &url,
        json!({"enabled": true, "maxSweeps": 2, "sweepCooldownMs": 25}),
    );

    let (status, body) = send_message(build_app(load_config(&config))).await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["type"], "rate_limit_error");
    // Only the first sweep dispatches; later sweeps skip the tier while the
    // recorded 429 backoff window is open.
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

/// maxHoldMs caps the wall-clock hold even with unlimited sweeps. The
/// first sweep takes at least one upstream round trip, so a 1ms budget is
/// already expired by the first decision point; the tolerance below only
/// allows for the (unlikely) sub-millisecond first sweep.
#[tokio::test]
async fn max_hold_deadline_surfaces_failure() {
    if skip_if_localhost_bind_unavailable("max_hold_deadline_surfaces_failure") {
        return;
    }
    let (url, hits) = spawn_flaky_upstream(usize::MAX, 500).await;
    let config = make_test_config(
        &url,
        json!({"enabled": true, "maxHoldMs": 1, "sweepCooldownMs": 25}),
    );

    let (status, _body) = send_message(build_app(load_config(&config))).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let hits = hits.load(Ordering::SeqCst);
    assert!(
        (1..=2).contains(&hits),
        "expected at most 2 upstream hits under a 1ms hold budget, got {hits}"
    );
}
