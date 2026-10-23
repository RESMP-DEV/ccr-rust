// SPDX-License-Identifier: AGPL-3.0-or-later
//! Offline config + live provider/listener health probe (`ccr-rust doctor`).
//!
//! This is a diagnostic aid, never a router hot path. It answers three
//! questions an operator otherwise learns only after a request fails:
//!
//! 1. Does the configuration parse and does every tier name a configured
//!    provider/model pair?
//! 2. Does each provider's credential actually authenticate, and does its
//!    live model catalog still list every model the config pins? A model the
//!    vendor rotated or unlisted (for example a `-Preview` id promoted to a
//!    stable id) is reported as `stale_model` rather than silently serving a
//!    different deployment.
//! 3. Is a listener running, and does its in-memory tier order match the file
//!    on disk? A stale in-memory order means a restart is pending.
//!
//! Probes reuse the exact header builders and endpoint URLs the dispatch path
//! uses, so a green doctor reflects what a real request would send. No probe
//! issues a completion: auth is checked with a catalog `GET` where the
//! provider implements one, and catalog comparison is skipped (reported
//! `no_catalog`) when the provider has no listing endpoint.

use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{Config, Provider, ProviderProtocol};
use crate::router::{
    build_anthropic_headers, build_openai_headers, canonical_model_name, provider_endpoint_url,
};

/// Per-provider probe outcome.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatus {
    /// Credential authenticated on the catalog listing endpoint.
    Ok,
    /// Provider has no catalog endpoint to probe; credential not exercised.
    NoCatalog,
    /// Credential rejected or transport failed; `detail` carries the reason.
    Error,
}

/// One provider's diagnosis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderReport {
    pub name: String,
    pub protocol: String,
    pub status: ProviderStatus,
    /// HTTP status of the catalog probe, when one was attempted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    /// Configured models the live catalog no longer lists. Empty for
    /// `NoCatalog` providers and when every model is still advertised.
    pub stale_models: Vec<String>,
    /// Models the config pins that were confirmed present in the catalog.
    pub live_models: Vec<String>,
    /// Whether the catalog comparison ran (false when the provider has no
    /// listing endpoint or the probe failed).
    pub catalog_checked: bool,
    pub detail: String,
}

/// A running listener's diagnosis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListenerReport {
    /// Whether the health endpoint transport answered, even with an HTTP error.
    pub reachable: bool,
    pub url: String,
    /// HTTP status when the endpoint answered; absent on transport failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    /// Whether `/health` itself returned a success response.
    pub healthy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_tiers: Option<Vec<String>>,
    /// True when the listener's in-memory tier order equals the configured
    /// backend tiers. False means the process predates a config change and a
    /// restart is pending.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier_order_matches: Option<bool>,
    pub detail: String,
}

/// Aggregate doctor result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub config_path: String,
    pub config_valid: bool,
    /// Tiers that do not name a configured provider/model pair.
    pub unresolved_tiers: Vec<String>,
    pub providers: Vec<ProviderReport>,
    pub listener: ListenerReport,
    /// Overall verdict: config valid, every provider non-error, and the
    /// listener (when reachable) matches the file. Stale models alone do not
    /// fail the run; they are surfaced as warnings.
    pub ok: bool,
    pub warnings: Vec<String>,
}

fn probe_headers(provider: &Provider) -> Result<reqwest::header::HeaderMap> {
    match provider.protocol {
        ProviderProtocol::Anthropic => {
            build_anthropic_headers(provider).map_err(|e| anyhow::anyhow!("{e}"))
        }
        ProviderProtocol::Openai | ProviderProtocol::Responses => {
            build_openai_headers(provider).map_err(|e| anyhow::anyhow!("{e}"))
        }
    }
}

/// Extract model ids from a catalog listing response. Handles both the OpenAI
/// `{"data":[{"id":...}]}` shape and a bare top-level array of ids or objects.
fn extract_catalog_ids(body: &Value) -> Vec<String> {
    fn collect(arr: &[Value], out: &mut Vec<String>) {
        for item in arr {
            if let Some(id) = item.get("id").and_then(Value::as_str) {
                out.push(id.to_string());
            } else if let Some(id) = item.as_str() {
                out.push(id.to_string());
            }
        }
    }
    let mut ids = Vec::new();
    if let Some(data) = body.get("data").and_then(Value::as_array) {
        collect(data, &mut ids);
    }
    if ids.is_empty() {
        if let Some(top) = body.as_array() {
            collect(top, &mut ids);
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

/// Match both the configured identity and the ID dispatch would send upstream.
fn model_is_live(provider: &Provider, model: &str, catalog: &[String]) -> bool {
    let wire_model = canonical_model_name(provider, model);
    catalog
        .iter()
        .any(|catalog_model| catalog_model == model || catalog_model == wire_model.as_ref())
}

/// Probe one provider's credential and compare its catalog to the pinned
/// models. Never issues a completion; uses the catalog `GET` only.
pub async fn probe_provider(
    client: &reqwest::Client,
    provider: &Provider,
    timeout: Duration,
) -> ProviderReport {
    let protocol = match provider.protocol {
        ProviderProtocol::Anthropic => "anthropic",
        ProviderProtocol::Openai => "openai",
        ProviderProtocol::Responses => "responses",
    };
    let base = ProviderReport {
        name: provider.name.clone(),
        protocol: protocol.to_string(),
        status: ProviderStatus::Error,
        http_status: None,
        stale_models: Vec::new(),
        live_models: Vec::new(),
        catalog_checked: false,
        detail: String::new(),
    };

    let headers = match probe_headers(provider) {
        Ok(h) => h,
        Err(e) => {
            return ProviderReport {
                detail: format!("cannot build auth headers: {e}"),
                ..base
            }
        }
    };

    // Catalog listing endpoint. Anthropic-compatible gateways that do not
    // implement /models answer 404/405; treat those as "no catalog" rather
    // than a credential failure, because the credential itself is unproven
    // either way and doctor must not fabricate a completion to check it.
    let url = provider_endpoint_url(provider, "models");
    let mut request = client.get(&url).header("accept", "application/json");
    if !headers.contains_key(reqwest::header::USER_AGENT) {
        request = request.header("user-agent", "ccr-rust-doctor/1");
    }
    let response = request.headers(headers).timeout(timeout).send().await;

    match response {
        Ok(resp) => {
            let status = resp.status();
            let code = status.as_u16();
            if status.is_success() {
                let body = resp.json::<Value>().await.unwrap_or(Value::Null);
                let catalog = extract_catalog_ids(&body);
                if catalog.is_empty() {
                    return ProviderReport {
                        status: ProviderStatus::NoCatalog,
                        http_status: Some(code),
                        detail:
                            "catalog endpoint returned no model ids; credential path not compared"
                                .into(),
                        ..base
                    };
                }
                let (live, stale): (Vec<String>, Vec<String>) = provider
                    .models
                    .iter()
                    .cloned()
                    .partition(|m| model_is_live(provider, m, &catalog));
                ProviderReport {
                    status: ProviderStatus::Ok,
                    http_status: Some(code),
                    stale_models: stale,
                    live_models: live,
                    catalog_checked: true,
                    detail: format!("authenticated on catalog ({} models listed)", catalog.len()),
                    ..base
                }
            } else if code == 404 || code == 405 {
                ProviderReport {
                    status: ProviderStatus::NoCatalog,
                    http_status: Some(code),
                    detail: format!("no catalog endpoint (HTTP {code}); credential not exercised"),
                    ..base
                }
            } else {
                ProviderReport {
                    status: ProviderStatus::Error,
                    http_status: Some(code),
                    detail: format!("catalog probe returned HTTP {code}"),
                    ..base
                }
            }
        }
        Err(e) => ProviderReport {
            status: ProviderStatus::Error,
            detail: format!("catalog probe failed: {e}"),
            ..base
        },
    }
}

/// Fetch a running listener's `/health` and compare its in-memory tier order
/// to the configured backend tiers.
pub async fn probe_listener(
    client: &reqwest::Client,
    host: &str,
    port: u16,
    expected_tiers: &[String],
    timeout: Duration,
) -> ListenerReport {
    let url = format!("http://{host}:{port}/health?details=1");
    match client.get(&url).timeout(timeout).send().await {
        Ok(resp) => {
            let code = resp.status().as_u16();
            let healthy = resp.status().is_success();
            let body = if healthy {
                resp.json::<Value>().await.unwrap_or(Value::Null)
            } else {
                Value::Null
            };
            // Plain "ok" is the stable liveness contract; current listeners
            // add tier details only when doctor opts in with ?details=1.
            let default_tier = body
                .get("default")
                .and_then(Value::as_str)
                .map(String::from);
            let live_tiers = body.get("tiers").and_then(Value::as_array).map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
            });
            let tier_order_matches = live_tiers
                .as_ref()
                .map(|live| live.as_slice() == expected_tiers);
            let detail = if !healthy {
                format!("/health returned HTTP {code}")
            } else if tier_order_matches == Some(false) {
                "listener is up but its in-memory tier order differs from the config file; a restart is pending".into()
            } else if live_tiers.is_none() {
                "listener is up; /health did not expose a tier list (older build)".into()
            } else {
                "listener is up and its tier order matches the config".into()
            };
            ListenerReport {
                reachable: true,
                url,
                http_status: Some(code),
                healthy,
                default_tier,
                live_tiers,
                tier_order_matches,
                detail,
            }
        }
        Err(e) => ListenerReport {
            reachable: false,
            url,
            http_status: None,
            healthy: false,
            default_tier: None,
            live_tiers: None,
            tier_order_matches: None,
            detail: format!("listener not reachable: {e}"),
        },
    }
}

/// Validate tiers against configured provider/model pairs without a network.
pub fn unresolved_tiers(config: &Config) -> Vec<String> {
    let mut unresolved = Vec::new();
    for tier in config.backend_tiers() {
        let provider = config.resolve_provider(&tier);
        let model_ok = tier
            .split_once(',')
            .map(|(_, model)| provider.is_some_and(|p| p.models.iter().any(|m| m == model)))
            .unwrap_or(false);
        if provider.is_none() || !model_ok {
            unresolved.push(tier.clone());
        }
    }
    unresolved
}

/// Run the full doctor: config validity, live provider probes, listener check.
pub async fn run(
    config_path: &str,
    host: &str,
    port: u16,
    timeout: Duration,
    skip_providers: bool,
) -> Result<DoctorReport> {
    let config = Config::from_file(config_path)?;
    let build_support_error = config.validate_build_support().err().map(|e| e.to_string());
    let config_valid = build_support_error.is_none();
    let expected_tiers: Vec<String> = config.backend_tiers();
    let unresolved = unresolved_tiers(&config);

    let client = reqwest::Client::new();
    let mut providers = Vec::new();
    if !skip_providers {
        for provider in config.providers() {
            providers.push(probe_provider(&client, provider, timeout).await);
        }
    }

    let listener = probe_listener(&client, host, port, &expected_tiers, timeout).await;

    let mut warnings = Vec::new();
    for p in &providers {
        if !p.stale_models.is_empty() {
            warnings.push(format!(
                "provider '{}' pins models absent from its live catalog: {} (rotated or unlisted id; verify before relying on it)",
                p.name,
                p.stale_models.join(", ")
            ));
        }
        if matches!(p.status, ProviderStatus::Error) {
            warnings.push(format!("provider '{}' probe failed: {}", p.name, p.detail));
        }
    }
    for tier in &unresolved {
        warnings.push(format!(
            "tier '{tier}' does not name a configured provider/model pair"
        ));
    }
    if let Some(error) = &build_support_error {
        warnings.push(format!(
            "configuration is not supported by this build: {error}"
        ));
    }
    if !listener.reachable {
        warnings.push(format!(
            "listener not reachable; provider/config checks are still valid: {}",
            listener.detail
        ));
    } else if !listener.healthy {
        warnings.push(format!("listener health probe failed: {}", listener.detail));
    } else if listener.tier_order_matches == Some(false) {
        warnings
            .push("listener tier order is stale; restart to activate the current config".into());
    }

    let providers_ok = providers
        .iter()
        .all(|p| !matches!(p.status, ProviderStatus::Error));
    let listener_ok =
        !listener.reachable || (listener.healthy && listener.tier_order_matches != Some(false));
    let ok = config_valid && unresolved.is_empty() && providers_ok && listener_ok;

    Ok(DoctorReport {
        config_path: config_path.to_string(),
        config_valid,
        unresolved_tiers: unresolved,
        providers,
        listener,
        ok,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provider(protocol: ProviderProtocol, models: &[&str]) -> Provider {
        serde_json::from_value(json!({
            "name": "test",
            "api_base_url": "https://api.example.test/v1",
            "api_key": "k",
            "protocol": match protocol {
                ProviderProtocol::Anthropic => "anthropic",
                ProviderProtocol::Responses => "responses",
                ProviderProtocol::Openai => "openai",
            },
            "models": models,
        }))
        .unwrap()
    }

    #[test]
    fn catalog_ids_from_openai_shape() {
        let ids = extract_catalog_ids(&json!({"data":[{"id":"a"},{"id":"b"}]}));
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn catalog_ids_from_bare_array() {
        let ids = extract_catalog_ids(&json!(["a", {"id": "b"}]));
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn stale_model_detected_against_catalog() {
        // The MiniMax flip class: config pins a preview id the vendor unlisted.
        let p = provider(ProviderProtocol::Openai, &["m-stable", "m-preview"]);
        let catalog = json!({"data":[{"id":"m-stable"},{"id":"m-other"}]});
        let ids = extract_catalog_ids(&catalog);
        let (live, stale): (Vec<_>, Vec<_>) = p
            .models
            .iter()
            .cloned()
            .partition(|m| ids.iter().any(|c| c == m));
        assert_eq!(live, vec!["m-stable".to_string()]);
        assert_eq!(stale, vec!["m-preview".to_string()]);
    }

    #[test]
    fn minimax_alias_matches_dispatch_canonical_catalog_id() {
        let p = provider(ProviderProtocol::Openai, &["MiniMax-M3"]);
        // The helper keeps the provider neutral; recognition follows the same
        // dispatch rule as production, including the MiniMax provider name.
        let minimax: Provider = serde_json::from_value(json!({
            "name": "minimax",
            "api_base_url": "https://api.minimax.chat/v1",
            "api_key": "k",
            "models": ["MiniMax-M3"],
        }))
        .unwrap();
        let catalog = vec!["MiniMax-M3.1-Flash".to_string()];
        assert!(model_is_live(&minimax, &p.models[0], &catalog));
        assert!(!model_is_live(&p, &p.models[0], &catalog));
    }

    #[tokio::test]
    async fn provider_probe_reports_unreachable_as_error() {
        let client = reqwest::Client::new();
        // Port 1 is reserved and never listening.
        let p: Provider = serde_json::from_value(json!({
            "name": "dead",
            "api_base_url": "http://127.0.0.1:1/v1",
            "api_key": "k",
            "models": ["x"],
        }))
        .unwrap();
        let report = probe_provider(&client, &p, Duration::from_millis(500)).await;
        assert_eq!(report.status, ProviderStatus::Error);
        assert!(!report.catalog_checked);
    }

    #[tokio::test]
    async fn listener_probe_reports_unreachable() {
        let client = reqwest::Client::new();
        let report = probe_listener(
            &client,
            "127.0.0.1",
            1,
            &["a,b".into()],
            Duration::from_millis(500),
        )
        .await;
        assert!(!report.reachable);
        assert!(report.detail.contains("not reachable"));
    }

    #[test]
    fn probe_headers_matches_protocol() {
        let anthropic = provider(ProviderProtocol::Anthropic, &["m"]);
        let h = probe_headers(&anthropic).unwrap();
        assert!(h.contains_key("x-api-key"));
        assert!(h.contains_key("anthropic-version"));
        let openai = provider(ProviderProtocol::Openai, &["m"]);
        let h = probe_headers(&openai).unwrap();
        assert!(h.contains_key("authorization"));
    }
}
