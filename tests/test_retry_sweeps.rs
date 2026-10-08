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
        admission_tracker: std::sync::Arc::new(ccr_rust::admission::AdmissionTracker::new()),
        retry_budget: std::sync::Arc::new(ccr_rust::retry_budget::RetryBudget::new()),
        sticky_sessions: std::sync::Arc::new(ccr_rust::stickiness::StickySessionTracker::new(
            std::time::Duration::from_secs(3600),
        )),
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

/// A client that gives up and disconnects must not leave an immortal
/// sweeping request behind: hyper/axum drops the handler future when the
/// connection closes, which cancels the cooldown sleep and stops further
/// upstream attempts. This pins that cancellation behavior — if a
/// hyper/axum upgrade ever stops cancelling pending handlers, abandoned
/// clients would become unbounded retry sources under unlimited sweeps.
#[tokio::test]
async fn client_disconnect_cancels_retry_sweeps() {
    if skip_if_localhost_bind_unavailable("client_disconnect_cancels_retry_sweeps") {
        return;
    }
    let (url, hits) = spawn_flaky_upstream(usize::MAX, 500).await;
    let config = make_test_config(&url, json!({"enabled": true, "sweepCooldownMs": 100}));
    let app = build_app(load_config(&config));

    // Serve the app over a real socket so the client can actually drop the
    // connection mid-hold (oneshot bypasses the transport entirely).
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let body = serde_json::to_vec(&json!({
        "model": "test-model",
        "messages": [{"role": "user", "content": "hello"}],
        "max_tokens": 32
    }))
    .unwrap();
    use tokio::io::AsyncWriteExt;
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!(
                "POST /v1/messages HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    stream.write_all(&body).await.unwrap();

    // Wait for at least two sweeps to fire while connected (poll rather
    // than sleep a fixed interval: under full-suite parallel load the first
    // dispatch can start arbitrarily late).
    let wait_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while hits.load(Ordering::SeqCst) < 2 {
        assert!(
            std::time::Instant::now() < wait_deadline,
            "upstream never received a sweep attempt while client was connected"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let hits_while_connected = hits.load(Ordering::SeqCst);
    drop(stream);

    // Wait until the hit count goes quiet (cancellation may take a moment
    // under load), then confirm growth stops at the disconnect point. One
    // in-flight upstream request may still complete after the drop —
    // cancelling the handler future does not un-send bytes already on the
    // wire — so the bound allows a single straggler, never continued
    // sweeping.
    let quiet_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let before = hits.load(Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let after = hits.load(Ordering::SeqCst);
        if before == after {
            assert!(
                after <= hits_while_connected + 1,
                "sweeps must stop after the client disconnects ({} hits before, {after} after)",
                hits_while_connected
            );
            break;
        }
        assert!(
            std::time::Instant::now() < quiet_deadline,
            "upstream hits kept growing after client disconnect: {hits_while_connected} -> {after}"
        );
    }
}

/// In a mixed cascade (one tier always 401, one tier rate-limited), the
/// deterministic tier is attempted exactly once for the whole request: the
/// first sweep rejects it, and later sweeps skip it while the request stays
/// held for the rate-limited tier's recovery.
#[tokio::test]
async fn deterministic_rejected_tier_skipped_in_later_sweeps() {
    if skip_if_localhost_bind_unavailable("deterministic_rejected_tier_skipped_in_later_sweeps") {
        return;
    }
    let (url_a, hits_a) = spawn_flaky_upstream(usize::MAX, 401).await;
    let (url_b, hits_b) = spawn_flaky_upstream(usize::MAX, 429).await;
    let config = json!({
        "Providers": [
            {
                "name": "mocka",
                "api_base_url": url_a,
                "api_key": "test-key",
                "models": ["test-model"]
            },
            {
                "name": "mockb",
                "api_base_url": url_b,
                "api_key": "test-key",
                "models": ["test-model"]
            }
        ],
        "Router": {
            "default": "mocka,test-model",
            "tiers": ["mocka,test-model", "mockb,test-model"],
            "tierRetries": {"mocka": {"max_retries": 0}, "mockb": {"max_retries": 0}},
            "retrySweeps": {"enabled": true, "maxSweeps": 2, "sweepCooldownMs": 25}
        },
        "API_TIMEOUT_MS": 5000
    });

    let config_json = serde_json::to_string_pretty(&config).unwrap();
    let (status, body) = send_message(build_app(load_config(&config_json))).await;

    // Terminal state reflects the last sweep: the rejected tier is skipped
    // silently by then, so only rate-limit signals remain and the
    // synthesized response is the rate-limit 429.
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["type"], "rate_limit_error");
    assert_eq!(
        hits_a.load(Ordering::SeqCst),
        1,
        "deterministically rejected tier must not be re-attempted in later sweeps"
    );
    let hits_b = hits_b.load(Ordering::SeqCst);
    assert!(
        (1..=2).contains(&hits_b),
        "rate-limited tier dispatches at most once per recovery window, got {hits_b}"
    );
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

/// Rate-limited tiers keep the request held and the inter-sweep cooldown
/// stretches to the tracker's actual backoff window (via the skip-path
/// hint), so the tier is re-attempted only once its recovery time elapses
/// rather than every bare sweepCooldownMs. Within two extra sweeps the
/// upstream is dispatched at most twice, and the final response is the
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

    let started = std::time::Instant::now();
    let (status, body) = send_message(build_app(load_config(&config))).await;
    let elapsed = started.elapsed();

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["type"], "rate_limit_error");
    let hits = hits.load(Ordering::SeqCst);
    assert!(
        (1..=2).contains(&hits),
        "expected 1-2 upstream hits (one per recovery window), got {hits}"
    );
    // A 429 without Retry-After paces the tier for 1s (no local escalation),
    // so two sweeps must take at least that long: at the configured 25ms
    // cooldown, finishing in under a second proves the sleep stretched to
    // the tracker's backoff window.
    assert!(
        elapsed >= std::time::Duration::from_millis(750),
        "cooldown must stretch to the tracker's backoff window; surfaced after {elapsed:?}"
    );
}

/// maxHoldMs caps the wall-clock hold even with unlimited sweeps. The cap
/// is enforced at sweep boundaries and the inter-sweep sleep is clamped to
/// the remaining budget, so a 1s budget with a 30s cooldown must surface
/// the failure after ~1s, not after the full 30s cooldown plus another
/// sweep: without the clamp the sleep itself would overshoot the deadline
/// by minutes. Exactly one hit pins the no-second-sweep invariant: the
/// loop-top check is gated on sweep > 0, so the first cascade always
/// dispatches regardless of pre-loop latency.
#[tokio::test]
async fn max_hold_deadline_surfaces_failure() {
    if skip_if_localhost_bind_unavailable("max_hold_deadline_surfaces_failure") {
        return;
    }
    let (url, hits) = spawn_flaky_upstream(usize::MAX, 500).await;
    let config = make_test_config(
        &url,
        json!({"enabled": true, "maxHoldMs": 1000, "sweepCooldownMs": 30000}),
    );

    let started = std::time::Instant::now();
    let (status, _body) = send_message(build_app(load_config(&config))).await;
    let elapsed = started.elapsed();

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "no second sweep may start once the hold budget is spent"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "cooldown must clamp to the remaining hold budget; surfaced after {elapsed:?} with a 1s budget"
    );
}

/// The sweepCooldownMs > 0 invariant is enforced at config load, so every
/// entry point (start, validate, dashboard) rejects a config that would
/// re-cascade back-to-back in an unbounded hot loop, not just the validate
/// subcommand.
#[test]
fn zero_sweep_cooldown_rejected_at_config_load() {
    let config = make_test_config(
        "http://127.0.0.1:1",
        json!({"enabled": true, "sweepCooldownMs": 0}),
    );
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, config).unwrap();

    let err = ccr_rust::config::Config::from_file(config_path.to_str().unwrap())
        .expect_err("enabled retrySweeps with sweepCooldownMs 0 must be rejected at load");
    assert!(
        err.to_string().contains("sweepCooldownMs"),
        "error should name the offending field: {err}"
    );

    // Disabled sweeps tolerate a zero cooldown: the value is inert.
    let config = make_test_config(
        "http://127.0.0.1:1",
        json!({"enabled": false, "sweepCooldownMs": 0}),
    );
    let config_path = dir.path().join("config-disabled.json");
    std::fs::write(&config_path, config).unwrap();
    assert!(
        ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).is_ok(),
        "disabled retrySweeps must not be rejected for a zero sweepCooldownMs"
    );
}
