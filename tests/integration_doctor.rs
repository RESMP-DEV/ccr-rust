// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration coverage for `ccr-rust doctor`: live provider catalog probing
//! and stale-model detection against a mocked upstream, plus listener drift.

use std::time::Duration;

use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use ccr_rust::doctor::{self, ProviderStatus};

fn write_config(dir: &std::path::Path, base_url: &str, models: &[&str]) -> String {
    let config = json!({
        "Providers": [{
            "name": "probe",
            "api_base_url": base_url,
            "api_key": "sk-test",
            "protocol": "openai",
            "models": models,
        }],
        "Router": {
            "default": format!("probe,{}", models[0]),
            "tiers": [format!("probe,{}", models[0])],
        },
        "PORT": 39999,
        "HOST": "127.0.0.1",
    });
    let path = dir.join("config.json");
    std::fs::write(&path, config.to_string()).unwrap();
    path.to_str().unwrap().to_string()
}

#[tokio::test]
async fn doctor_flags_model_absent_from_live_catalog() {
    let server = MockServer::start().await;
    // Catalog lists only the stable id; the pinned preview id is gone.
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data": [{"id": "m-stable"}, {"id": "m-other"}]})),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let cfg = write_config(dir.path(), &server.uri(), &["m-stable", "m-preview"]);

    let report = doctor::run(&cfg, "127.0.0.1", 39999, Duration::from_secs(5), false)
        .await
        .unwrap();

    let probe = report
        .providers
        .iter()
        .find(|p| p.name == "probe")
        .expect("probe provider present");
    assert_eq!(probe.status, ProviderStatus::Ok);
    assert!(probe.catalog_checked);
    assert_eq!(probe.stale_models, vec!["m-preview".to_string()]);
    assert_eq!(probe.live_models, vec!["m-stable".to_string()]);
    assert!(report
        .warnings
        .iter()
        .any(|w| w.contains("m-preview") && w.contains("absent from its live catalog")));
    // A stale pin is a warning, not a hard failure: the run is still "ok"
    // overall because the credential authenticated and the config is valid.
    assert!(report.ok);
}

#[tokio::test]
async fn doctor_reports_all_models_live_when_catalog_matches() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data": [{"id": "m-stable"}, {"id": "m-preview"}]})),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let cfg = write_config(dir.path(), &server.uri(), &["m-stable", "m-preview"]);

    let report = doctor::run(&cfg, "127.0.0.1", 39999, Duration::from_secs(5), false)
        .await
        .unwrap();
    let probe = &report.providers[0];
    assert_eq!(probe.status, ProviderStatus::Ok);
    assert!(probe.stale_models.is_empty());
    assert_eq!(probe.live_models.len(), 2);
    assert!(report.ok);
}

#[tokio::test]
async fn doctor_reports_no_catalog_when_listing_returns_404() {
    let server = MockServer::start().await;
    // Anthropic-compatible gateways that lack /models answer 404.
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "not found"})))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let cfg = write_config(dir.path(), &server.uri(), &["m-stable"]);

    let report = doctor::run(&cfg, "127.0.0.1", 39999, Duration::from_secs(5), false)
        .await
        .unwrap();
    let probe = &report.providers[0];
    assert_eq!(probe.status, ProviderStatus::NoCatalog);
    assert_eq!(probe.http_status, Some(404));
    assert!(!probe.catalog_checked);
    assert!(probe.stale_models.is_empty());
    assert!(report.ok);
}

#[tokio::test]
async fn doctor_detects_credential_rejection() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": "bad key"})))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let cfg = write_config(dir.path(), &server.uri(), &["m-stable"]);

    let report = doctor::run(&cfg, "127.0.0.1", 39999, Duration::from_secs(5), false)
        .await
        .unwrap();
    let probe = &report.providers[0];
    assert_eq!(probe.status, ProviderStatus::Error);
    assert_eq!(probe.http_status, Some(401));
    assert!(!report.ok);
}

#[tokio::test]
async fn doctor_reports_unresolved_tier_without_network() {
    let dir = tempfile::tempdir().unwrap();
    // Tier names a model the provider does not list.
    let config = json!({
        "Providers": [{
            "name": "probe",
            "api_base_url": "http://127.0.0.1:1/v1",
            "api_key": "sk-test",
            "protocol": "openai",
            "models": ["m-stable"],
        }],
        "Router": {"default": "probe,m-stable", "tiers": ["probe,m-missing"]},
        "PORT": 39999, "HOST": "127.0.0.1",
    });
    let path = dir.path().join("config.json");
    std::fs::write(&path, config.to_string()).unwrap();

    let report = doctor::run(
        path.to_str().unwrap(),
        "127.0.0.1",
        39999,
        Duration::from_secs(2),
        true, // skip provider network probes
    )
    .await
    .unwrap();
    assert_eq!(report.unresolved_tiers, vec!["probe,m-missing".to_string()]);
    assert!(!report.ok);
    assert!(report.providers.is_empty());
}

#[tokio::test]
async fn doctor_preserves_provider_user_agent_requirements() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("user-agent", "required-agent/1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": "m-stable"}]})),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let config = json!({
        "Providers": [{
            "name": "probe",
            "api_base_url": server.uri(),
            "api_key": "sk-test",
            "protocol": "openai",
            "models": ["m-stable"],
            "extra_headers": {"User-Agent": "required-agent/1"},
        }],
        "Router": {"default": "probe,m-stable", "tiers": ["probe,m-stable"]},
        "PORT": 39999,
        "HOST": "127.0.0.1",
    });
    let path = dir.path().join("config.json");
    std::fs::write(&path, config.to_string()).unwrap();

    let report = doctor::run(
        path.to_str().unwrap(),
        "127.0.0.1",
        39999,
        Duration::from_secs(5),
        false,
    )
    .await
    .unwrap();
    assert_eq!(report.providers[0].status, ProviderStatus::Ok);
}

#[tokio::test]
async fn doctor_detects_listener_tier_drift() {
    let listener = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "ok",
            "default": "probe,m-old",
            "tiers": ["probe,m-old"],
        })))
        .mount(&listener)
        .await;
    let address = listener.address();

    let dir = tempfile::tempdir().unwrap();
    let cfg = write_config(dir.path(), "http://127.0.0.1:1/v1", &["m-new"]);
    let report = doctor::run(
        &cfg,
        &address.ip().to_string(),
        address.port(),
        Duration::from_secs(5),
        true,
    )
    .await
    .unwrap();
    assert!(report.listener.reachable);
    assert!(report.listener.healthy);
    assert_eq!(report.listener.tier_order_matches, Some(false));
    assert!(!report.ok);
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.contains("tier order is stale")));
}

#[tokio::test]
async fn doctor_fails_listener_health_http_error() {
    let listener = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&listener)
        .await;
    let address = listener.address();

    let dir = tempfile::tempdir().unwrap();
    let cfg = write_config(dir.path(), "http://127.0.0.1:1/v1", &["m-stable"]);
    let report = doctor::run(
        &cfg,
        &address.ip().to_string(),
        address.port(),
        Duration::from_secs(5),
        true,
    )
    .await
    .unwrap();
    assert!(report.listener.reachable);
    assert_eq!(report.listener.http_status, Some(503));
    assert!(!report.listener.healthy);
    assert!(!report.ok);
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.contains("listener health probe failed")));
}

#[cfg(feature = "gp")]
#[tokio::test]
async fn doctor_reports_gp_runtime_configuration_errors() {
    let dir = tempfile::tempdir().unwrap();
    let config = json!({
        "Providers": [{
            "name": "probe",
            "api_base_url": "http://127.0.0.1:1/v1",
            "api_key": "sk-test",
            "protocol": "openai",
            "models": ["m-stable"],
        }],
        "Router": {
            "default": "probe,m-stable",
            "tiers": ["probe,m-stable"],
            "gpRouting": {"enabled": true, "kplsDim": 0},
        },
        "PORT": 39999,
        "HOST": "127.0.0.1",
    });
    let path = dir.path().join("config.json");
    std::fs::write(&path, config.to_string()).unwrap();

    let report = doctor::run(
        path.to_str().unwrap(),
        "127.0.0.1",
        39999,
        Duration::from_secs(2),
        true,
    )
    .await
    .unwrap();
    assert!(!report.config_valid);
    assert!(!report.ok);
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.contains("kplsDim")));
}
