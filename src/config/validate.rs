// SPDX-License-Identifier: AGPL-3.0-or-later
//! Configuration validation gates applied by `Config::from_file`.
//!
//! Split out of `config/mod.rs` per the SLOC ratchet (split a hotspot
//! before growing it). Every gate here runs for every entry point (start,
//! validate, doctor), not just the validate subcommand.

use anyhow::Result;

use super::plugins::{PluginOnError, PluginStages, PluginsConfig};
use super::types::{ModelAliasTarget, Provider, ProviderProtocol, RouterConfig};
use super::ConfigFile;

pub(super) const REASONING_EFFORT_VALUES: &[&str] =
    &["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Refuse to start when a provider credential failed environment expansion.
///
/// shellexpand only substitutes variables present in the process
/// environment. A router started outside the credential launcher (raw
/// `ccr-rust start` instead of `serve.py`) keeps the literal placeholder as
/// the key, and every provider then returns 401s indistinguishable from
/// expired credentials, which derails diagnosis (2026-09-23 incident).
/// The gate consumes the recorded expansion failures, so values that
/// expanded successfully are never rejected for their content.
/// Set `CCR_ALLOW_UNEXPANDED_CREDENTIALS=true` to override for
/// intentionally keyless local setups.
pub(super) fn validate_provider_credentials(
    config: &serde_json::Value,
    expansion_failures: &[(String, String)],
    allow_unexpanded: bool,
) -> Result<()> {
    if allow_unexpanded {
        return Ok(());
    }
    let providers = config
        .get("Providers")
        .and_then(serde_json::Value::as_array);
    let mut offenders: Vec<String> = Vec::new();
    if let Some(providers) = providers {
        for (index, provider) in providers.iter().enumerate() {
            let name = provider
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?");
            let api_key_pointer = format!("/Providers/{index}/api_key");
            let header_prefix = format!("/Providers/{index}/extra_headers/");
            for (pointer, _error) in expansion_failures {
                if pointer == &api_key_pointer {
                    offenders.push(format!("provider '{name}' api_key"));
                } else if let Some(header) = pointer.strip_prefix(&header_prefix) {
                    offenders.push(format!("provider '{name}' header '{header}'"));
                }
            }
        }
    }
    if offenders.is_empty() {
        return Ok(());
    }
    offenders.sort();
    offenders.dedup();
    anyhow::bail!(
        "unexpanded credential references: {}. Start via the credential launcher (serve.py) \
         or export the referenced variables; requests would authenticate with the literal \
         placeholder and every provider would return 401. \
         Set CCR_ALLOW_UNEXPANDED_CREDENTIALS=true to override.",
        offenders.join(", ")
    );
}

pub(super) fn validate_provider_extra_headers(config: &serde_json::Value) -> Result<()> {
    let Some(providers) = config.get("Providers").and_then(|value| value.as_array()) else {
        return Ok(());
    };
    for (index, provider) in providers.iter().enumerate() {
        let Some(headers) = provider
            .get("extra_headers")
            .and_then(|value| value.as_object())
        else {
            continue;
        };
        let name = provider
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        for (header, value) in headers {
            reqwest::header::HeaderName::from_bytes(header.as_bytes()).map_err(|error| {
                anyhow::anyhow!(
                    "invalid extra_headers key for provider '{name}' at /Providers/{index}/{header}: {error}"
                )
            })?;
            let header_value = value.as_str().ok_or_else(|| {
                anyhow::anyhow!(
                    "invalid extra_headers value for provider '{name}' at /Providers/{index}/{header}: value must be a string"
                )
            })?;
            header_value
                .parse::<reqwest::header::HeaderValue>()
                .map_err(|error| {
                    anyhow::anyhow!(
                        "invalid extra_headers value for provider '{name}' at /Providers/{index}/{header}: {error}"
                    )
                })?;
        }
    }
    Ok(())
}

pub(super) fn validate_client_credentials(
    config: &serde_json::Value,
    expansion_failures: &[(String, String)],
    allow_unexpanded: bool,
) -> Result<()> {
    if allow_unexpanded {
        return Ok(());
    }
    if config.get("CLIENT_API_KEY").is_some()
        && expansion_failures
            .iter()
            .any(|(pointer, _error)| pointer == "/CLIENT_API_KEY")
    {
        anyhow::bail!(
            "unexpanded client API key reference: /CLIENT_API_KEY. Start via the credential \
             launcher or export CCR_CLIENT_API_KEY before exposing the API. Set \
             CCR_ALLOW_UNEXPANDED_CREDENTIALS=true to override for a loopback-only setup."
        );
    }
    Ok(())
}

pub(super) fn validate_client_listener(file: &ConfigFile) -> Result<()> {
    anyhow::ensure!(
        file.client_api_key.is_some() == file.auth_port.is_some(),
        "CLIENT_API_KEY and AUTH_PORT must be configured together: the normal HOST/PORT \
         listener stays unauthenticated for local clients, while AUTH_PORT requires the key"
    );
    anyhow::ensure!(
        file.auth_port != Some(file.port),
        "AUTH_PORT must differ from PORT so the authenticated endpoint cannot displace or gate \
         the existing local listener"
    );
    anyhow::ensure!(
        file.auth_host.parse::<std::net::IpAddr>().is_ok(),
        "AUTH_HOST must be an IP address"
    );
    Ok(())
}

/// Validate cross-field provider requirements before the router accepts traffic.
pub(super) fn validate_provider_contracts(providers: &[Provider]) -> Result<()> {
    for provider in providers {
        if let Some(max_inflight) = provider.max_inflight {
            anyhow::ensure!(
                (1..=64).contains(&max_inflight),
                "provider '{}' maxInflight must be between 1 and 64, got {}",
                provider.name,
                max_inflight
            );
        }
        if let Some(max_inflight_tokens) = provider.max_inflight_tokens {
            anyhow::ensure!(
                (1_000..=4_000_000).contains(&max_inflight_tokens),
                "provider '{}' maxInflightTokens must be between 1000 and 4000000, got {}",
                provider.name,
                max_inflight_tokens
            );
        }
        let Some(reasoning_effort) = provider.force_reasoning_effort.as_deref() else {
            continue;
        };
        if provider.protocol != ProviderProtocol::Openai {
            anyhow::bail!(
                "provider '{}' force_reasoning_effort requires protocol 'openai'",
                provider.name
            );
        }
        if !REASONING_EFFORT_VALUES.contains(&reasoning_effort) {
            anyhow::bail!(
                "provider '{}' has invalid force_reasoning_effort '{}'; expected one of: {}",
                provider.name,
                reasoning_effort,
                REASONING_EFFORT_VALUES.join(", ")
            );
        }
    }
    Ok(())
}

/// Enforced in `Config::from_file` so every entry point (start, validate,
/// dashboard) rejects it, not just the validate subcommand: a zero
/// sweepCooldownMs would re-cascade the tier list back-to-back in an
/// unbounded hot loop whenever no Retry-After hint or hold cap applies.
pub(super) fn validate_retry_sweeps(router: &RouterConfig) -> Result<()> {
    let sweeps = &router.retry_sweeps;
    anyhow::ensure!(
        !(sweeps.enabled && sweeps.sweep_cooldown_ms == 0),
        "Router.retrySweeps.sweepCooldownMs must be > 0 when retrySweeps is enabled \
         (zero would re-cascade the tier list back-to-back in an unbounded hot loop)"
    );
    Ok(())
}

pub(super) fn validate_retry_budget(router: &RouterConfig) -> Result<()> {
    anyhow::ensure!(
        router.retry_budget_percent <= 100,
        "Router.retryBudgetPercent must be between 0 and 100, got {}",
        router.retry_budget_percent
    );
    Ok(())
}

/// Names of plugins that fail the request stage.
///
/// A hedge is built from the request before the primary's request stage has
/// finished, and it runs its own tier chain. A fail-mode request plugin
/// therefore cannot be honoured on the hedge path: the hedge tier may omit the
/// plugin and send the untransformed payload.
pub(super) fn fail_mode_request_plugins(plugins: &PluginsConfig) -> Vec<String> {
    plugins
        .plugins
        .iter()
        .filter(|(_, spec)| {
            matches!(spec.on_error, PluginOnError::Fail)
                && matches!(spec.stages, PluginStages::Request | PluginStages::Both)
        })
        .map(|(name, _)| name.clone())
        .collect()
}

pub(super) fn validate_hedging(router: &RouterConfig, plugins: &PluginsConfig) -> Result<()> {
    anyhow::ensure!(
        !(router.hedging.enabled && router.hedging.ttft_threshold_ms == 0),
        "Router.hedging.ttftThresholdMs must be > 0 when hedging is enabled"
    );
    if router.hedging.enabled {
        let conflicting = fail_mode_request_plugins(plugins);
        anyhow::ensure!(
            conflicting.is_empty(),
            "Router.hedging cannot be enabled with a fail-mode request plugin ({}): \
             a hedge is built from the untransformed request and could send it to a tier \
             whose chain omits the plugin",
            conflicting.join(", ")
        );
    }
    Ok(())
}

/// strictTierOrder serves the entire configured tier chain in order, so
/// topK (which truncates the EWMA-sampled candidate set) has no meaning
/// alongside it. Reject the combination instead of silently ignoring one of
/// two contradictory candidate-set controls.
pub(super) fn validate_tier_order_conflicts(router: &RouterConfig) -> Result<()> {
    anyhow::ensure!(
        !(router.strict_tier_order && router.top_k.is_some()),
        "Router.strictTierOrder and Router.topK cannot be combined: strict ordering \
         serves the full configured tier chain, so topK would be silently ignored"
    );
    Ok(())
}

pub(super) fn validate_model_aliases(router: &RouterConfig, providers: &[Provider]) -> Result<()> {
    for (alias, target) in &router.model_aliases {
        anyhow::ensure!(
            !alias.trim().is_empty() && alias == alias.trim() && !alias.contains(','),
            "Router.modelAliases alias '{alias}' must be a nonblank bare model name without surrounding whitespace"
        );
        let route = target.route();
        anyhow::ensure!(
            route == route.trim(),
            "Router.modelAliases target '{route}' has surrounding whitespace"
        );
        let Some((provider, model)) = route.split_once(',') else {
            anyhow::bail!(
                "Router.modelAliases target '{route}' must be an explicit provider,model route"
            );
        };
        anyhow::ensure!(
            !provider.is_empty()
                && !model.is_empty()
                && !model.contains(',')
                && provider == provider.trim()
                && model == model.trim()
                && providers.iter().any(|candidate| candidate.name == provider
                    && candidate
                        .models
                        .iter()
                        .any(|configured| configured == model)),
            "Router.modelAliases target '{route}' must name a configured provider,model route"
        );
        if let ModelAliasTarget::Detailed {
            display_name,
            description,
            ..
        } = target
        {
            for (field, value) in [("display_name", display_name), ("description", description)] {
                if let Some(text) = value {
                    anyhow::ensure!(
                        !text.trim().is_empty() && text == text.trim(),
                        "Router.modelAliases alias '{alias}' {field} must be nonblank without surrounding whitespace"
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugins(value: serde_json::Value) -> PluginsConfig {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn fail_mode_request_plugins_are_detected() {
        let config = plugins(serde_json::json!({
            "plugins": {
                "redact": {"command": "/bin/cat", "on_error": "fail"},
                "shape": {"command": "/bin/cat", "on_error": "fail", "stages": "response"},
                "optional": {"command": "/bin/cat"}
            }
        }));
        let mut names = fail_mode_request_plugins(&config);
        names.sort();
        assert_eq!(names, vec!["redact".to_string()]);
    }
}
