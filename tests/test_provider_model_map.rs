// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration coverage for the provider-level model alias map (`modelMap`).
//!
//! A retired model id can be aliased onto its successor at dispatch time, so
//! a client pin such as `zai,glm-5.3` reaches the provider as `glm-5.4`
//! without any client-side pin changing. Every upstream here is a local mock
//! that records the `model` field it received.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tower::ServiceExt;

/// Skip integration tests that require opening localhost sockets.
fn skip_if_localhost_bind_unavailable(test_name: &str) -> bool {
    if std::net::TcpListener::bind("127.0.0.1:0").is_ok() {
        return false;
    }
    eprintln!("Skipping {test_name}: cannot bind localhost sockets");
    true
}

/// OpenAI-compatible upstream that records the `model` field of every
/// request it serves.
async fn spawn_recording_upstream() -> (String, Arc<Mutex<Vec<String>>>) {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    async fn record(State(seen): State<Arc<Mutex<Vec<String>>>>, body: Json<Value>) -> Json<Value> {
        let model = body
            .0
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        seen.lock().unwrap().push(model);

        // The upstream names no model of its own, so the model the client
        // sees is the one the router decided to put on the wire.
        Json(json!({
            "id": "resp_model_map",
            "object": "chat.completion",
            "created": 1,
            "model": "",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        }))
    }

    let app = Router::new()
        .route("/chat/completions", post(record))
        .with_state(seen.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    (format!("http://{addr}"), seen)
}

/// Config with one provider whose model map aliases retired ids onto the
/// models the provider actually serves.
fn provider_config(url: &str, models: &[&str], model_map: Value, default_tier: &str) -> Value {
    json!({
        "Providers": [{
            "name": "zai",
            "api_base_url": url,
            "api_key": "test-key",
            "models": models,
            "modelMap": model_map
        }],
        "Router": {"default": default_tier},
        "API_TIMEOUT_MS": 5000
    })
}

/// Load a config through the normal entry point, returning the error text
/// when the configuration is rejected.
fn load_config_result(config_json: &Value) -> Result<ccr_rust::config::Config, String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, serde_json::to_vec(config_json).unwrap()).unwrap();
    ccr_rust::config::Config::from_file(path.to_str().unwrap()).map_err(|e| e.to_string())
}

/// Build the Axum app with test state.
fn build_app(config: ccr_rust::config::Config) -> Router {
    let state = ccr_rust::router::AppState {
        config,
        ewma_tracker: Arc::new(ccr_rust::routing::EwmaTracker::new()),
        gp_router: None,
        transformer_registry: Arc::new(ccr_rust::transformer::TransformerRegistry::new()),
        active_streams: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
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
        .with_state(state)
}

/// Send an Anthropic-shaped client request and return the status and body.
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
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!({"raw_body": String::from_utf8_lossy(&bytes).to_string()}));
    (status, value)
}

/// A retired model id pinned by a client is aliased onto its successor: the
/// upstream receives the mapped id and the client response reports it.
#[tokio::test]
async fn provider_model_map_aliases_retired_model_for_upstream() {
    if skip_if_localhost_bind_unavailable("provider_model_map_aliases_retired_model_for_upstream") {
        return;
    }

    let (url, seen) = spawn_recording_upstream().await;
    let config = load_config_result(&provider_config(
        &url,
        &["glm-5.4"],
        json!({"glm-5.3": "glm-5.4"}),
        "zai,glm-5.3",
    ))
    .expect("config with a registered model map target must load");
    let app = build_app(config);

    let (status, body) = send(
        &app,
        json!({
            "model": "zai,glm-5.3",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 64
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        seen.lock().unwrap().clone(),
        vec!["glm-5.4".to_string()],
        "upstream must receive the mapped model id"
    );
    assert_eq!(
        body.get("model").and_then(Value::as_str),
        Some("glm-5.4"),
        "client response must report the mapped model"
    );
}

/// A model the map does not mention keeps its requested id on the wire, so an
/// alias never rewrites a route the operator did not retire.
#[tokio::test]
async fn provider_model_map_leaves_unmapped_model_unchanged() {
    if skip_if_localhost_bind_unavailable("provider_model_map_leaves_unmapped_model_unchanged") {
        return;
    }

    let (url, seen) = spawn_recording_upstream().await;
    let config = load_config_result(&provider_config(
        &url,
        &["glm-5.4", "glm-5.5"],
        json!({"glm-5.3": "glm-5.4"}),
        "zai,glm-5.5",
    ))
    .expect("config with a registered model map target must load");
    let app = build_app(config);

    let (status, body) = send(
        &app,
        json!({
            "model": "zai,glm-5.5",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 64
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        seen.lock().unwrap().clone(),
        vec!["glm-5.5".to_string()],
        "an unmapped model must reach the upstream unchanged"
    );
    assert_eq!(
        body.get("model").and_then(Value::as_str),
        Some("glm-5.5"),
        "client response must report the requested model"
    );
}

/// The configuration entry point rejects a map that points at a model the
/// provider does not serve.
#[test]
fn config_entry_point_rejects_model_map_with_unknown_target() {
    let error = load_config_result(&provider_config(
        "http://127.0.0.1:9/v1",
        &["glm-5.4"],
        json!({"glm-5.3": "glm-5.9"}),
        "zai,glm-5.3",
    ))
    .expect_err("a model map target outside the provider models list must be rejected");

    assert!(
        error.contains("modelMap target 'glm-5.9' for requested model 'glm-5.3'"),
        "unexpected error: {error}"
    );
}

/// The configuration entry point rejects a map that aliases a model onto
/// itself, which would be a silent no-op.
#[test]
fn config_entry_point_rejects_self_mapped_model() {
    let error = load_config_result(&provider_config(
        "http://127.0.0.1:9/v1",
        &["glm-5.4"],
        json!({"glm-5.4": "glm-5.4"}),
        "zai,glm-5.4",
    ))
    .expect_err("a self-mapped model must be rejected");

    assert!(
        error.contains("modelMap entry 'glm-5.4' must not map a model onto itself"),
        "unexpected error: {error}"
    );
}
