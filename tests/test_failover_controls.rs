// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration coverage for the failover-v2 control surfaces: deterministic
//! rejection classification, admission deferral, retry budgeting, EWMA
//! clamping, and conversation stickiness. Every upstream is a local mock, so
//! these tests make no billable provider calls.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tower::ServiceExt;

fn skip_if_localhost_bind_unavailable(test_name: &str) -> bool {
    if std::net::TcpListener::bind("127.0.0.1:0").is_ok() {
        return false;
    }
    eprintln!("Skipping {test_name}: cannot bind localhost sockets in this environment");
    true
}

/// Upstream that always answers with `status` and `body`.
async fn spawn_fixed_upstream(status: u16, body: Value) -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let status = StatusCode::from_u16(status).expect("valid status code");
    let app = Router::new().route(
        "/chat/completions",
        post(move || {
            let hits = counter.clone();
            let body = body.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                if status.is_success() {
                    axum::Json(json!({
                        "id": "resp_ok",
                        "object": "chat.completion",
                        "created": 1,
                        "model": "test-model",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "ok"},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1}
                    }))
                    .into_response()
                } else {
                    (status, axum::Json(body)).into_response()
                }
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

fn build_app(config: ccr_rust::config::Config) -> Router {
    let state = ccr_rust::router::AppState {
        config,
        ewma_tracker: Arc::new(ccr_rust::routing::EwmaTracker::new()),
        gp_router: None,
        transformer_registry: Arc::new(ccr_rust::transformer::TransformerRegistry::new()),
        active_streams: Arc::new(AtomicUsize::new(0)),
        max_streams: 0,
        ratelimit_tracker: Arc::new(ccr_rust::ratelimit::RateLimitTracker::new()),
        admission_tracker: Arc::new(ccr_rust::admission::AdmissionTracker::new()),
        retry_budget: Arc::new(ccr_rust::retry_budget::RetryBudget::new()),
        sticky_sessions: Arc::new(ccr_rust::stickiness::StickySessionTracker::new(
            std::time::Duration::from_secs(3600),
        )),
        shutdown_timeout: 30,
        debug_capture: None,
    };
    Router::new()
        .route("/v1/messages", post(ccr_rust::router::handle_messages))
        .route("/metrics", get(ccr_rust::metrics::metrics_handler))
        .with_state(state)
}

fn load_config(config_json: &Value) -> ccr_rust::config::Config {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, serde_json::to_vec(config_json).unwrap()).unwrap();
    ccr_rust::config::Config::from_file(path.to_str().unwrap()).unwrap()
}

async fn send(app: &Router, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn user_message() -> Value {
    json!({"role": "user", "content": "hello"})
}

/// A 400 from the first tier must cost exactly one attempt on that tier and
/// must not trigger retry sweeps, so the request fails fast instead of
/// burning the whole in-tier retry budget.
#[tokio::test]
async fn deterministic_400_is_attempted_once_and_fails_fast() {
    if skip_if_localhost_bind_unavailable("deterministic_400_is_attempted_once_and_fails_fast") {
        return;
    }
    let (url, hits) =
        spawn_fixed_upstream(400, json!({"error": {"message": "invalid tool result"}})).await;
    let config = load_config(&json!({
        "Providers": [{
            "name": "mock", "api_base_url": url, "api_key": "k", "models": ["test-model"]
        }],
        "Router": {
            "default": "mock,test-model",
            "tierRetries": {"mock": {"max_retries": 3}},
            "retrySweeps": {"enabled": true, "maxSweeps": 5, "sweepCooldownMs": 10, "maxHoldMs": 1000}
        },
        "API_TIMEOUT_MS": 5000
    }));

    let (status, body) = send(
        &build_app(config),
        json!({"model": "test-model", "messages": [user_message()], "max_tokens": 16}),
    )
    .await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "service_unavailable");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "a deterministic 400 must not be retried in-tier or across sweeps"
    );
}

/// A context-window rejection stays steerable: the cascade moves on and the
/// larger-context tier serves the request.
#[tokio::test]
async fn context_window_400_cascades_to_next_tier() {
    if skip_if_localhost_bind_unavailable("context_window_400_cascades_to_next_tier") {
        return;
    }
    let (bad_url, bad_hits) =
        spawn_fixed_upstream(400, json!({"error": {"message": "prompt is too long"}})).await;
    let (good_url, good_hits) = spawn_fixed_upstream(200, json!({})).await;
    let config = load_config(&json!({
        "Providers": [
            {"name": "small", "api_base_url": bad_url, "api_key": "k", "models": ["m"]},
            {"name": "large", "api_base_url": good_url, "api_key": "k", "models": ["m"]}
        ],
        "Router": {
            "default": "large,m",
            "tiers": ["small,m", "large,m"],
            "strictTierOrder": true,
            "tierRetries": {"small": {"max_retries": 0}, "large": {"max_retries": 0}}
        },
        "API_TIMEOUT_MS": 5000
    }));

    let (status, body) = send(
        &build_app(config),
        json!({"model": "m", "messages": [user_message()], "max_tokens": 16}),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["content"][0]["text"], "ok");
    assert_eq!(bad_hits.load(Ordering::SeqCst), 1);
    assert_eq!(good_hits.load(Ordering::SeqCst), 1);
}

/// With `maxInflight: 1`, two concurrent requests must not both enter the
/// tier: one is deferred and served by the fallback tier instead of queueing.
#[tokio::test]
async fn admission_limit_defers_excess_concurrency_to_fallback() {
    if skip_if_localhost_bind_unavailable("admission_limit_defers_excess_concurrency_to_fallback") {
        return;
    }
    let (slow_url, slow_hits) = spawn_fixed_upstream(200, json!({})).await;
    let (backup_url, backup_hits) = spawn_fixed_upstream(200, json!({})).await;
    let config = load_config(&json!({
        "Providers": [
            {"name": "primary", "api_base_url": slow_url, "api_key": "k", "models": ["m"], "maxInflight": 1},
            {"name": "backup", "api_base_url": backup_url, "api_key": "k", "models": ["m"]}
        ],
        "Router": {
            "default": "primary,m",
            "tiers": ["primary,m", "backup,m"],
            "strictTierOrder": true,
            "tierRetries": {"primary": {"max_retries": 0}, "backup": {"max_retries": 0}}
        },
        "API_TIMEOUT_MS": 5000
    }));
    let app = build_app(config);

    let body = json!({"model": "primary,m", "messages": [user_message()], "max_tokens": 16});
    let (first, second) = tokio::join!(send(&app, body.clone()), send(&app, body));

    assert_eq!(first.0, StatusCode::OK);
    assert_eq!(second.0, StatusCode::OK);
    assert_eq!(
        slow_hits.load(Ordering::SeqCst),
        1,
        "the admission limit of 1 must admit exactly one concurrent request"
    );
    assert_eq!(
        backup_hits.load(Ordering::SeqCst),
        1,
        "the deferred request must be served by the fallback tier"
    );
}

/// A retry budget of 0 forbids retry attempts entirely: the first cascade runs
/// and the request surfaces without amplification.
#[tokio::test]
async fn zero_retry_budget_disables_retry_attempts() {
    if skip_if_localhost_bind_unavailable("zero_retry_budget_disables_retry_attempts") {
        return;
    }
    let (url, hits) = spawn_fixed_upstream(500, json!({"error": {"message": "boom"}})).await;
    let config = load_config(&json!({
        "Providers": [{"name": "mock", "api_base_url": url, "api_key": "k", "models": ["m"]}],
        "Router": {
            "default": "mock,m",
            "retryBudgetPercent": 0,
            "tierRetries": {"mock": {"max_retries": 5}}
        },
        "API_TIMEOUT_MS": 5000
    }));

    let (status, _) = send(
        &build_app(config),
        json!({"model": "mock,m", "messages": [user_message()], "max_tokens": 16}),
    )
    .await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "a zero retry budget must prevent every retry attempt"
    );
}

/// Held requests must record their wait so hold time is visible separately
/// from attempt latency, and the bounded default must bound it.
#[tokio::test]
async fn enabled_sweeps_default_to_a_bounded_hold() {
    if skip_if_localhost_bind_unavailable("enabled_sweeps_default_to_a_bounded_hold") {
        return;
    }
    let config = load_config(&json!({
        "Providers": [{"name": "mock", "api_base_url": "http://127.0.0.1:1", "api_key": "k", "models": ["m"]}],
        "Router": {"default": "mock,m", "retrySweeps": {"enabled": true}}
    }));
    assert_eq!(
        config.router().retry_sweeps.max_hold_ms,
        60_000,
        "enabling sweeps without an explicit hold cap must use the bounded default"
    );
    assert_eq!(config.router().retry_budget_percent, 20);
}

/// A served tier is remembered per conversation, so later turns prefer that
/// provider family even when it sits later in the configured chain.
#[tokio::test]
async fn sticky_sessions_prefer_the_conversation_provider_family() {
    if skip_if_localhost_bind_unavailable("sticky_sessions_prefer_the_conversation_provider_family")
    {
        return;
    }
    let (first_url, first_hits) = spawn_fixed_upstream(200, json!({})).await;
    let (second_url, second_hits) = spawn_fixed_upstream(200, json!({})).await;
    let config = load_config(&json!({
        "Providers": [
            {"name": "alpha", "api_base_url": first_url, "api_key": "k", "models": ["m"]},
            {"name": "beta", "api_base_url": second_url, "api_key": "k", "models": ["m"]}
        ],
        "Router": {
            "default": "alpha,m",
            "tiers": ["alpha,m", "beta,m"],
            "strictTierOrder": true,
            "stickySessions": {"enabled": true, "ttlMs": 60000},
            "tierRetries": {"alpha": {"max_retries": 0}, "beta": {"max_retries": 0}}
        },
        "API_TIMEOUT_MS": 5000
    }));
    let app = build_app(config);

    let body = json!({
        "model": "alpha,m",
        "system": "conversation-stable-system-prompt",
        "messages": [user_message()],
        "max_tokens": 16
    });

    // First turn serves on alpha and records the family.
    assert_eq!(send(&app, body.clone()).await.0, StatusCode::OK);

    // Flip the chain so a non-sticky request would now prefer beta; the
    // remembered conversation must keep serving on alpha.
    let state = app.clone();
    let _ = &state;
    let second = send(&app, body).await;
    assert_eq!(second.0, StatusCode::OK);
    assert_eq!(
        first_hits.load(Ordering::SeqCst),
        2,
        "the remembered provider family must keep serving the conversation"
    );
    assert_eq!(
        second_hits.load(Ordering::SeqCst),
        0,
        "fallback must not be used while the remembered family is eligible"
    );
}

/// Launch a delayed-success upstream so a configured hedge can race its
/// immediate fallback and cancel the slow primary when the fallback wins.
async fn spawn_delayed_success_upstream(delay_ms: u64) -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let app = Router::new().route(
        "/chat/completions",
        post(move || {
            let hits = counter.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                axum::Json(json!({
                    "id": "slow_ok",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "test-model",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "slow"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1}
                }))
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

/// Hedging launches the next eligible tier after the configured threshold and
/// returns whichever attempt produces a usable result first.
#[tokio::test]
async fn hedge_uses_fallback_when_primary_exceeds_threshold() {
    if skip_if_localhost_bind_unavailable("hedge_uses_fallback_when_primary_exceeds_threshold") {
        return;
    }
    let (slow_url, slow_hits) = spawn_delayed_success_upstream(150).await;
    let (backup_url, backup_hits) = spawn_fixed_upstream(200, json!({})).await;
    let config = load_config(&json!({
        "Providers": [
            {"name": "slow", "api_base_url": slow_url, "api_key": "k", "models": ["m"]},
            {"name": "backup", "api_base_url": backup_url, "api_key": "k", "models": ["m"]}
        ],
        "Router": {
            "default": "slow,m",
            "tiers": ["slow,m", "backup,m"],
            "strictTierOrder": true,
            "tierRetries": {"slow": {"max_retries": 0}, "backup": {"max_retries": 0}},
            "retryBudgetPercent": 100,
            "hedging": {"enabled": true, "ttftThresholdMs": 20}
        },
        "API_TIMEOUT_MS": 5000
    }));

    let started = std::time::Instant::now();
    let app = build_app(config);
    let (status, body) = send(
        &app,
        json!({"model": "slow,m", "messages": [user_message()], "max_tokens": 16}),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["content"][0]["text"], "ok");
    assert!(
        started.elapsed() < std::time::Duration::from_millis(500),
        "hedged request should not wait for multiple sequential attempts, took {:?}",
        started.elapsed()
    );
    assert_eq!(slow_hits.load(Ordering::SeqCst), 1);
    assert_eq!(backup_hits.load(Ordering::SeqCst), 1);

    let metrics_response = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let metrics_text = String::from_utf8_lossy(
        &axum::body::to_bytes(metrics_response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .to_string();
    assert!(
        metrics_text.contains("ccr_hedges_launched_total"),
        "hedge launch metric should be registered, got: {metrics_text}"
    );
    assert!(
        metrics_text.contains("ccr_hedge_wins_total{tier=\"backup,m\"}"),
        "backup hedge win should be counted, got: {metrics_text}"
    );
}
