// SPDX-License-Identifier: AGPL-3.0-or-later
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware;
use axum::routing::{get, post};
use axum::Router;
use serde_json::json;
use tower::ServiceExt;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
        .route("/v1/models", get(ccr_rust::router::list_models))
        .route("/health", get(|| async { "ok" }))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            ccr_rust::router::require_client_auth,
        ))
        .with_state(state)
}

#[tokio::test]
async fn client_api_key_gates_routes_but_not_health() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_partial_json(json!({"model": "test-model"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "auth-test",
            "object": "chat.completion",
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })))
        .expect(2)
        .mount(&upstream)
        .await;

    let config_json = json!({
        "CLIENT_API_KEY": "test-client-key",
        "Providers": [{
            "name": "mock",
            "api_base_url": upstream.uri(),
            "api_key": "upstream-key",
            "models": ["test-model"]
        }],
        "Router": {"default": "mock,test-model"}
    });
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.json");
    std::fs::write(&config_path, serde_json::to_vec(&config_json).unwrap()).unwrap();
    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let health = app
        .clone()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);

    let body = serde_json::to_vec(&json!({
        "model": "mock,test-model",
        "max_tokens": 32,
        "messages": [{"role": "user", "content": "hello"}]
    }))
    .unwrap();

    let unauthorized = app
        .clone()
        .oneshot(
            Request::post("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let bearer = app
        .clone()
        .oneshot(
            Request::post("/v1/messages")
                .header("content-type", "application/json")
                .header("authorization", "Bearer test-client-key")
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bearer.status(), StatusCode::OK);

    let anthropic_key = app
        .oneshot(
            Request::post("/v1/messages")
                .header("content-type", "application/json")
                .header("x-api-key", "test-client-key")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(anthropic_key.status(), StatusCode::OK);

    upstream.verify().await;
}
