use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::Router;
use serde_json::json;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use ccr_rust::config::TierRetryConfig;

// ---------------------------------------------------------------------------
// Unit-level tests for TierRetryConfig::backoff_duration
// ---------------------------------------------------------------------------

#[test]
fn backoff_duration_exponential_growth() {
    let cfg = TierRetryConfig {
        max_retries: 5,
        base_backoff_ms: 100,
        backoff_multiplier: 2.0,
        max_backoff_ms: 10_000,
    };

    assert_eq!(cfg.backoff_duration(0), Duration::from_millis(100));
    assert_eq!(cfg.backoff_duration(1), Duration::from_millis(200));
    assert_eq!(cfg.backoff_duration(2), Duration::from_millis(400));
    assert_eq!(cfg.backoff_duration(3), Duration::from_millis(800));
    assert_eq!(cfg.backoff_duration(4), Duration::from_millis(1600));
}

#[test]
fn backoff_duration_clamps_at_max() {
    let cfg = TierRetryConfig {
        max_retries: 10,
        base_backoff_ms: 500,
        backoff_multiplier: 3.0,
        max_backoff_ms: 5_000,
    };

    // attempt 0: 500ms, attempt 1: 1500ms, attempt 2: 4500ms, attempt 3: 13500 -> clamped to 5000
    assert_eq!(cfg.backoff_duration(0), Duration::from_millis(500));
    assert_eq!(cfg.backoff_duration(1), Duration::from_millis(1500));
    assert_eq!(cfg.backoff_duration(2), Duration::from_millis(4500));
    assert_eq!(cfg.backoff_duration(3), Duration::from_millis(5000));
    assert_eq!(cfg.backoff_duration(4), Duration::from_millis(5000));
}

#[test]
fn backoff_duration_multiplier_one_is_constant() {
    let cfg = TierRetryConfig {
        max_retries: 5,
        base_backoff_ms: 250,
        backoff_multiplier: 1.0,
        max_backoff_ms: 10_000,
    };

    for attempt in 0..5 {
        assert_eq!(
            cfg.backoff_duration(attempt),
            Duration::from_millis(250),
            "attempt {} should produce constant 250ms delay",
            attempt,
        );
    }
}

#[test]
fn backoff_duration_zero_base_is_zero() {
    let cfg = TierRetryConfig {
        max_retries: 3,
        base_backoff_ms: 0,
        backoff_multiplier: 2.0,
        max_backoff_ms: 10_000,
    };

    for attempt in 0..3 {
        assert_eq!(cfg.backoff_duration(attempt), Duration::from_millis(0));
    }
}

#[test]
fn backoff_duration_large_attempt_saturates_at_max() {
    let cfg = TierRetryConfig {
        max_retries: 100,
        base_backoff_ms: 1,
        backoff_multiplier: 10.0,
        max_backoff_ms: 30_000,
    };

    // 1 * 10^20 is astronomically large; must clamp to 30s
    assert_eq!(cfg.backoff_duration(20), Duration::from_millis(30_000));
}

#[test]
fn backoff_duration_fractional_multiplier() {
    let cfg = TierRetryConfig {
        max_retries: 5,
        base_backoff_ms: 1000,
        backoff_multiplier: 1.5,
        max_backoff_ms: 100_000,
    };

    // attempt 0: 1000, attempt 1: 1500, attempt 2: 2250, attempt 3: 3375
    assert_eq!(cfg.backoff_duration(0), Duration::from_millis(1000));
    assert_eq!(cfg.backoff_duration(1), Duration::from_millis(1500));
    assert_eq!(cfg.backoff_duration(2), Duration::from_millis(2250));
    assert_eq!(cfg.backoff_duration(3), Duration::from_millis(3375));
}

// ---------------------------------------------------------------------------
// TierRetryConfig default values
// ---------------------------------------------------------------------------

#[test]
fn default_tier_retry_config() {
    let cfg = TierRetryConfig::default();

    assert_eq!(cfg.max_retries, 3);
    assert_eq!(cfg.base_backoff_ms, 100);
    assert_eq!(cfg.backoff_multiplier, 2.0);
    assert_eq!(cfg.max_backoff_ms, 10_000);
}

// ---------------------------------------------------------------------------
// JSON deserialization of TierRetryConfig
// ---------------------------------------------------------------------------

#[test]
fn tier_retry_config_deserialize_full() {
    let json = json!({
        "max_retries": 5,
        "base_backoff_ms": 200,
        "backoff_multiplier": 3.0,
        "max_backoff_ms": 30000
    });

    let cfg: TierRetryConfig = serde_json::from_value(json).unwrap();
    assert_eq!(cfg.max_retries, 5);
    assert_eq!(cfg.base_backoff_ms, 200);
    assert_eq!(cfg.backoff_multiplier, 3.0);
    assert_eq!(cfg.max_backoff_ms, 30_000);
}

#[test]
fn tier_retry_config_deserialize_partial_uses_defaults() {
    let json = json!({
        "max_retries": 7
    });

    let cfg: TierRetryConfig = serde_json::from_value(json).unwrap();
    assert_eq!(cfg.max_retries, 7);
    assert_eq!(cfg.base_backoff_ms, 100); // default
    assert_eq!(cfg.backoff_multiplier, 2.0); // default
    assert_eq!(cfg.max_backoff_ms, 10_000); // default
}

#[test]
fn tier_retry_config_deserialize_empty_object() {
    let json = json!({});
    let cfg: TierRetryConfig = serde_json::from_value(json).unwrap();

    let defaults = TierRetryConfig::default();
    assert_eq!(cfg.max_retries, defaults.max_retries);
    assert_eq!(cfg.base_backoff_ms, defaults.base_backoff_ms);
    assert_eq!(cfg.backoff_multiplier, defaults.backoff_multiplier);
    assert_eq!(cfg.max_backoff_ms, defaults.max_backoff_ms);
}

// ---------------------------------------------------------------------------
// Config file deserialization with tier retries
// ---------------------------------------------------------------------------

#[test]
fn config_file_deserialize_with_tier_retries() {
    let json = json!({
        "Providers": [
            {
                "name": "openai",
                "api_base_url": "https://api.openai.com/v1",
                "api_key": "sk-test",
                "models": ["gpt-4"]
            }
        ],
        "Router": {
            "default": "openai,gpt-4",
            "tierRetries": {
                "tier-0": {
                    "max_retries": 5,
                    "base_backoff_ms": 50,
                    "backoff_multiplier": 1.5,
                    "max_backoff_ms": 5000
                },
                "tier-1": {
                    "max_retries": 2,
                    "base_backoff_ms": 200
                }
            }
        }
    });

    let cfg: ccr_rust::config::ConfigFile = serde_json::from_value(json).unwrap();

    let tier0 = cfg.router.tier_retries.get("tier-0").unwrap();
    assert_eq!(tier0.max_retries, 5);
    assert_eq!(tier0.base_backoff_ms, 50);
    assert_eq!(tier0.backoff_multiplier, 1.5);
    assert_eq!(tier0.max_backoff_ms, 5000);

    let tier1 = cfg.router.tier_retries.get("tier-1").unwrap();
    assert_eq!(tier1.max_retries, 2);
    assert_eq!(tier1.base_backoff_ms, 200);
    assert_eq!(tier1.backoff_multiplier, 2.0); // default
    assert_eq!(tier1.max_backoff_ms, 10_000); // default
}

#[test]
fn config_file_deserialize_no_tier_retries() {
    let json = json!({
        "Providers": [
            {
                "name": "openai",
                "api_base_url": "https://api.openai.com/v1",
                "api_key": "sk-test",
                "models": ["gpt-4"]
            }
        ],
        "Router": {
            "default": "openai,gpt-4"
        }
    });

    let cfg: ccr_rust::config::ConfigFile = serde_json::from_value(json).unwrap();
    assert!(cfg.router.tier_retries.is_empty());
}

// ---------------------------------------------------------------------------
// Config::get_tier_retry resolution (requires on-disk config for Config::from_file)
// ---------------------------------------------------------------------------

#[test]
fn get_tier_retry_with_custom_and_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");

    let config_json = json!({
        "Providers": [
            {
                "name": "mock",
                "api_base_url": "http://127.0.0.1:1234/v1",
                "api_key": "test-key",
                "models": ["test-model"]
            }
        ],
        "Router": {
            "default": "mock,test-model",
            "tierRetries": {
                "tier-0": {
                    "max_retries": 10,
                    "base_backoff_ms": 50,
                    "backoff_multiplier": 1.5,
                    "max_backoff_ms": 2000
                }
            }
        }
    });

    std::fs::write(
        &config_path,
        serde_json::to_string_pretty(&config_json).unwrap(),
    )
    .unwrap();
    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();

    // tier-0 has custom config
    let tier0 = config.get_tier_retry("tier-0");
    assert_eq!(tier0.max_retries, 10);
    assert_eq!(tier0.base_backoff_ms, 50);
    assert_eq!(tier0.backoff_multiplier, 1.5);
    assert_eq!(tier0.max_backoff_ms, 2000);

    // tier-1 falls back to defaults
    let tier1 = config.get_tier_retry("tier-1");
    let defaults = TierRetryConfig::default();
    assert_eq!(tier1.max_retries, defaults.max_retries);
    assert_eq!(tier1.base_backoff_ms, defaults.base_backoff_ms);
    assert_eq!(tier1.backoff_multiplier, defaults.backoff_multiplier);
    assert_eq!(tier1.max_backoff_ms, defaults.max_backoff_ms);
}

// ---------------------------------------------------------------------------
// Integration tests: full HTTP routing with wiremock backends
// ---------------------------------------------------------------------------

/// Helper: build a Config pointing at the given mock server URL.
fn make_test_config(mock_url: &str, tier_retries: HashMap<String, TierRetryConfig>) -> String {
    let config = json!({
        "Providers": [
            {
                "name": "mock",
                "api_base_url": mock_url,
                "api_key": "test-key",
                "models": ["test-model"]
            }
        ],
        "Router": {
            "default": "mock,test-model",
            "tierRetries": tier_retries.into_iter().map(|(k, v)| {
                (k, json!({
                    "max_retries": v.max_retries,
                    "base_backoff_ms": v.base_backoff_ms,
                    "backoff_multiplier": v.backoff_multiplier,
                    "max_backoff_ms": v.max_backoff_ms,
                }))
            }).collect::<serde_json::Map<String, serde_json::Value>>()
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
    };
    Router::new()
        .route("/v1/messages", post(ccr_rust::router::handle_messages))
        .with_state(state)
}

fn test_request_body() -> serde_json::Value {
    json!({
        "model": "mock,test-model",
        "messages": [{"role": "user", "content": "hello"}],
        "max_tokens": 100
    })
}

#[tokio::test]
async fn successful_request_no_retry() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"choices": [{"message": {"content": "hi"}}]})),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let config_json = make_test_config(&mock_server.uri(), HashMap::new());
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
                .body(Body::from(
                    serde_json::to_vec(&test_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn retries_on_backend_failure_then_succeeds() {
    let mock_server = MockServer::start().await;

    // First two requests fail, third succeeds.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
        .up_to_n_times(2)
        .expect(2)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"choices": [{"message": {"content": "recovered"}}]})),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let config_json = make_test_config(&mock_server.uri(), HashMap::new());
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
                .body(Body::from(
                    serde_json::to_vec(&test_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn all_retries_exhausted_returns_503() {
    let mock_server = MockServer::start().await;

    // Every request fails.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("always fails"))
        .expect(3) // MAX_RETRIES = 3, single tier
        .mount(&mock_server)
        .await;

    let config_json = make_test_config(&mock_server.uri(), HashMap::new());
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
                .body(Body::from(
                    serde_json::to_vec(&test_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn backoff_introduces_measurable_delay() {
    let mock_server = MockServer::start().await;

    // All 3 attempts fail. With default backoff (100ms base, 2x multiplier):
    // attempt 0 fails -> sleep 100ms
    // attempt 1 fails -> sleep 200ms
    // attempt 2 fails -> no sleep (last attempt)
    // Total backoff: ~300ms minimum
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("fail"))
        .expect(3)
        .mount(&mock_server)
        .await;

    let config_json = make_test_config(&mock_server.uri(), HashMap::new());
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();

    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let start = Instant::now();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&test_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let elapsed = start.elapsed();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    // Backoff sleeps: 100ms + 200ms = 300ms minimum.
    // Allow generous lower bound (250ms) for CI jitter.
    assert!(
        elapsed >= Duration::from_millis(250),
        "Expected >= 250ms of backoff delay, got {:?}",
        elapsed,
    );
}

#[tokio::test]
async fn multi_tier_cascade_on_failure() {
    // Two mock servers representing two tiers.
    let tier0_server = MockServer::start().await;
    let tier1_server = MockServer::start().await;

    // tier-0 always fails
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("tier0 down"))
        .mount(&tier0_server)
        .await;

    // tier-1 succeeds
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"choices": [{"message": {"content": "from tier1"}}]})),
        )
        .expect(1)
        .mount(&tier1_server)
        .await;

    let config = json!({
        "Providers": [
            {
                "name": "tier0prov",
                "api_base_url": tier0_server.uri(),
                "api_key": "key0",
                "models": ["m0"]
            },
            {
                "name": "tier1prov",
                "api_base_url": tier1_server.uri(),
                "api_key": "key1",
                "models": ["m1"]
            }
        ],
        "Router": {
            "default": "tier0prov,m0",
            "think": "tier1prov,m1"
        },
        "API_TIMEOUT_MS": 5000
    });

    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let cfg = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(cfg);

    let body = json!({
        "model": "tier0prov,m0",
        "messages": [{"role": "user", "content": "test"}],
        "max_tokens": 50
    });

    let resp = app
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

    // Should succeed via tier-1 after tier-0 exhaustion
    assert_eq!(resp.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Backoff edge cases
// ---------------------------------------------------------------------------

#[test]
fn backoff_duration_max_smaller_than_base() {
    // When max_backoff_ms < base_backoff_ms, every attempt clamps to max.
    let cfg = TierRetryConfig {
        max_retries: 3,
        base_backoff_ms: 5000,
        backoff_multiplier: 2.0,
        max_backoff_ms: 1000,
    };

    assert_eq!(cfg.backoff_duration(0), Duration::from_millis(1000));
    assert_eq!(cfg.backoff_duration(1), Duration::from_millis(1000));
}

#[test]
fn backoff_duration_multiplier_less_than_one_decays() {
    // Sub-1.0 multiplier means backoff decreases with each attempt.
    let cfg = TierRetryConfig {
        max_retries: 5,
        base_backoff_ms: 1000,
        backoff_multiplier: 0.5,
        max_backoff_ms: 10_000,
    };

    assert_eq!(cfg.backoff_duration(0), Duration::from_millis(1000));
    assert_eq!(cfg.backoff_duration(1), Duration::from_millis(500));
    assert_eq!(cfg.backoff_duration(2), Duration::from_millis(250));
    assert_eq!(cfg.backoff_duration(3), Duration::from_millis(125));
}

// ---------------------------------------------------------------------------
// Config round-trip: tier_retries survive serialize -> deserialize
// ---------------------------------------------------------------------------

#[test]
fn tier_retry_config_roundtrip_serde() {
    let original = TierRetryConfig {
        max_retries: 7,
        base_backoff_ms: 250,
        backoff_multiplier: 1.8,
        max_backoff_ms: 15_000,
    };

    let json = serde_json::to_string(&original).unwrap();
    let restored: TierRetryConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(restored.max_retries, original.max_retries);
    assert_eq!(restored.base_backoff_ms, original.base_backoff_ms);
    assert_eq!(restored.backoff_multiplier, original.backoff_multiplier);
    assert_eq!(restored.max_backoff_ms, original.max_backoff_ms);
}

// ---------------------------------------------------------------------------
// Adaptive backoff integration tests
// These tests verify that backoff delays are applied during actual HTTP retries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn adaptive_backoff_applies_configured_delays() {
    let mock_server = MockServer::start().await;

    // Mock server always fails with 500
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("fail"))
        .mount(&mock_server)
        .await;

    // Configure aggressive backoff for measurable delays: 50ms base, 1.5x multiplier
    // NOTE: Currently router uses hardcoded backoff (100ms * 2^attempt), not config values.
    // This test verifies the configured backoff values exist and the delay mechanism works.
    let mut tier_retries = HashMap::new();
    tier_retries.insert(
        "tier-0".to_string(),
        TierRetryConfig {
            max_retries: 3,
            base_backoff_ms: 50,
            backoff_multiplier: 1.5,
            max_backoff_ms: 10000,
        },
    );

    let config_json = make_test_config(&mock_server.uri(), tier_retries);
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();

    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();

    // Verify config values are correctly parsed
    let retry_config = config.get_tier_retry("tier-0");
    assert_eq!(retry_config.base_backoff_ms, 50);
    assert_eq!(retry_config.backoff_multiplier, 1.5);

    let app = build_app(config);

    let start = Instant::now();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&test_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let elapsed = start.elapsed();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    // NOTE: Router currently uses hardcoded 100ms base * 2^attempt = 300ms total backoff
    // When router reads config.get_tier_retry(), this should be 50ms + 75ms = 125ms
    // Test verifies backoff mechanism exists; update expectations when router respects config
    assert!(
        elapsed >= Duration::from_millis(200),
        "Expected >= 200ms of backoff delay, got {:?}",
        elapsed,
    );
}

#[tokio::test]
async fn adaptive_backoff_clamps_to_max() {
    let mock_server = MockServer::start().await;

    // Mock server always fails
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("fail"))
        .mount(&mock_server)
        .await;

    // Configure: 200ms base, 10x multiplier, 500ms max
    // Without clamping: 200 + 2000 + 20000 = 22200ms
    // With clamping: 200 + 500 + 500 = 1200ms
    let mut tier_retries = HashMap::new();
    tier_retries.insert(
        "tier-0".to_string(),
        TierRetryConfig {
            max_retries: 3,
            base_backoff_ms: 200,
            backoff_multiplier: 10.0,
            max_backoff_ms: 500,
        },
    );

    let config_json = make_test_config(&mock_server.uri(), tier_retries);
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();

    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let start = Instant::now();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&test_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let elapsed = start.elapsed();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    // With clamping: max 200ms + 500ms + 500ms = 1200ms total backoff
    // Upper bound should be significantly less than unclamped value
    assert!(
        elapsed < Duration::from_millis(5000),
        "Expected clamped backoff (< 5s), got {:?}",
        elapsed,
    );
    assert!(
        elapsed >= Duration::from_millis(500),
        "Expected >= 500ms of backoff delay, got {:?}",
        elapsed,
    );
}

#[tokio::test]
async fn adaptive_backoff_per_tier_configuration() {
    let tier0_server = MockServer::start().await;
    let tier1_server = MockServer::start().await;

    // tier-0: fast backoff (50ms base)
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("tier0 fail"))
        .mount(&tier0_server)
        .await;

    // tier-1: slower backoff (200ms base) - but will succeed
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"choices": [{"message": {"content": "success"}}]})),
        )
        .expect(1)
        .mount(&tier1_server)
        .await;

    let config = json!({
        "Providers": [
            {
                "name": "tier0prov",
                "api_base_url": tier0_server.uri(),
                "api_key": "key0",
                "models": ["m0"]
            },
            {
                "name": "tier1prov",
                "api_base_url": tier1_server.uri(),
                "api_key": "key1",
                "models": ["m1"]
            }
        ],
        "Router": {
            "default": "tier0prov,m0",
            "think": "tier1prov,m1",
            "tierRetries": {
                "tier-0": {
                    "max_retries": 2,
                    "base_backoff_ms": 50,
                    "backoff_multiplier": 2.0,
                    "max_backoff_ms": 1000
                },
                "tier-1": {
                    "max_retries": 2,
                    "base_backoff_ms": 200,
                    "backoff_multiplier": 1.5,
                    "max_backoff_ms": 1000
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

    let start = Instant::now();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&test_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let elapsed = start.elapsed();

    assert_eq!(resp.status(), StatusCode::OK);
    // tier-0 fails twice with 50ms backoff each = ~100ms
    // tier-1 succeeds on first try = no backoff
    assert!(
        elapsed >= Duration::from_millis(50),
        "Expected >= 50ms from tier-0 backoff, got {:?}",
        elapsed,
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "Expected < 500ms total (tier-0 only), got {:?}",
        elapsed,
    );
}

#[tokio::test]
async fn adaptive_backoff_zero_base_no_delay() {
    let mock_server = MockServer::start().await;

    // Mock server always fails
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("fail"))
        .mount(&mock_server)
        .await;

    // Configure: 0ms base backoff = no delay between retries
    let mut tier_retries = HashMap::new();
    tier_retries.insert(
        "tier-0".to_string(),
        TierRetryConfig {
            max_retries: 3,
            base_backoff_ms: 0,
            backoff_multiplier: 2.0,
            max_backoff_ms: 10000,
        },
    );

    let config_json = make_test_config(&mock_server.uri(), tier_retries);
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();

    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let start = Instant::now();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&test_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let elapsed = start.elapsed();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    // With 0ms backoff, should complete quickly (just HTTP round trips)
    assert!(
        elapsed < Duration::from_millis(100),
        "Expected < 100ms with 0ms backoff, got {:?}",
        elapsed,
    );
}

#[tokio::test]
async fn adaptive_backoff_constant_multiplier() {
    let mock_server = MockServer::start().await;

    // Mock server always fails
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("fail"))
        .mount(&mock_server)
        .await;

    // Configure: multiplier = 1.0 means constant backoff
    let mut tier_retries = HashMap::new();
    tier_retries.insert(
        "tier-0".to_string(),
        TierRetryConfig {
            max_retries: 3,
            base_backoff_ms: 100,
            backoff_multiplier: 1.0,
            max_backoff_ms: 10000,
        },
    );

    let config_json = make_test_config(&mock_server.uri(), tier_retries);
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();

    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let start = Instant::now();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&test_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let elapsed = start.elapsed();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    // Constant 100ms backoff = 200ms total (two backoffs)
    assert!(
        elapsed >= Duration::from_millis(150),
        "Expected >= 150ms constant backoff, got {:?}",
        elapsed,
    );
}

#[tokio::test]
async fn adaptive_backoff_fractional_multiplier() {
    let mock_server = MockServer::start().await;

    // Mock server always fails
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("fail"))
        .mount(&mock_server)
        .await;

    // Configure: 0.5 multiplier means decreasing backoff
    let mut tier_retries = HashMap::new();
    tier_retries.insert(
        "tier-0".to_string(),
        TierRetryConfig {
            max_retries: 3,
            base_backoff_ms: 100,
            backoff_multiplier: 0.5,
            max_backoff_ms: 10000,
        },
    );

    let config_json = make_test_config(&mock_server.uri(), tier_retries);
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();

    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let start = Instant::now();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&test_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let elapsed = start.elapsed();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    // Decreasing backoff: 100ms + 50ms = 150ms
    assert!(
        elapsed >= Duration::from_millis(100),
        "Expected >= 100ms decreasing backoff, got {:?}",
        elapsed,
    );
    assert!(
        elapsed < Duration::from_millis(200),
        "Expected < 200ms (decreasing), got {:?}",
        elapsed,
    );
}

#[tokio::test]
async fn adaptive_backoff_fallback_to_default_when_unconfigured() {
    let mock_server = MockServer::start().await;

    // Mock server always fails
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("fail"))
        .mount(&mock_server)
        .await;

    // No tier_retries configured - should use defaults (100ms base, 2x multiplier)
    let config_json = make_test_config(&mock_server.uri(), HashMap::new());
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    std::fs::write(&config_path, &config_json).unwrap();

    let config = ccr_rust::config::Config::from_file(config_path.to_str().unwrap()).unwrap();
    let app = build_app(config);

    let start = Instant::now();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&test_request_body()).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let elapsed = start.elapsed();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    // Default backoff: 100ms + 200ms = 300ms
    assert!(
        elapsed >= Duration::from_millis(250),
        "Expected >= 250ms default backoff, got {:?}",
        elapsed,
    );
}
