// SPDX-License-Identifier: AGPL-3.0-or-later
mod types;
pub use types::*;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::sync::Arc;

use crate::debug_capture::DebugCaptureConfig;

const REASONING_EFFORT_VALUES: &[&str] =
    &["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Recursively expand `${VAR}` references in every string value of a parsed
/// JSON document. Values whose variables are missing keep their raw text and
/// record the failure; `validate_provider_credentials` decides whether the
/// leftovers are acceptable.
fn expand_env_references(value: &mut serde_json::Value, failures: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => match shellexpand::env(text) {
            Ok(expanded) => *text = expanded.into_owned(),
            Err(error) => failures.push(error.to_string()),
        },
        serde_json::Value::Array(items) => {
            for item in items {
                expand_env_references(item, failures);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values_mut() {
                expand_env_references(item, failures);
            }
        }
        _ => {}
    }
}

/// Detect a leftover environment reference in either shellexpand syntax:
/// `${VAR}` or braceless `$VAR`. Used to reject credentials that would be
/// sent as literal placeholder text.
pub(crate) fn contains_env_placeholder(text: &str) -> bool {
    if text.contains("${") {
        return true;
    }
    let bytes = text.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'$' {
            continue;
        }
        let mut cursor = index + 1;
        if cursor >= bytes.len() {
            break;
        }
        let first = bytes[cursor];
        if !(first.is_ascii_alphabetic() || first == b'_') {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len()
            && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'_')
        {
            cursor += 1;
        }
        return true;
    }
    false
}

/// Refuse to start with credentials that still contain unexpanded
/// environment references.
///
/// shellexpand only substitutes variables present in the process
/// environment. A router started outside the credential launcher (raw
/// `ccr-rust start` instead of `serve.py`) keeps the literal placeholder as
/// the key, and every provider then returns 401s indistinguishable from
/// expired credentials, which derails diagnosis (2026-09-23 incident).
/// Failing fast at startup turns that into an immediate, self-explaining
/// error. Set `CCR_ALLOW_UNEXPANDED_CREDENTIALS=true` to override for
/// intentionally keyless local setups.
fn validate_provider_credentials(file: &ConfigFile, allow_unexpanded: bool) -> Result<()> {
    if allow_unexpanded {
        return Ok(());
    }
    let mut offenders: Vec<String> = Vec::new();
    for provider in &file.providers {
        if contains_env_placeholder(&provider.api_key) {
            offenders.push(format!("provider '{}' api_key", provider.name));
        }
        if let Some(headers) = &provider.extra_headers {
            for (header_name, value) in headers {
                if contains_env_placeholder(value) {
                    offenders.push(format!(
                        "provider '{}' header '{}'",
                        provider.name, header_name
                    ));
                }
            }
        }
    }
    if offenders.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "unexpanded credential references: {}. Start via the credential launcher (serve.py) \
         or export the referenced variables; requests would authenticate with the literal \
         placeholder and every provider would return 401. \
         Set CCR_ALLOW_UNEXPANDED_CREDENTIALS=true to override.",
        offenders.join(", ")
    );
}

/// Validate cross-field provider requirements before the router accepts traffic.
fn validate_provider_contracts(providers: &[Provider]) -> Result<()> {    for provider in providers {
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

fn validate_model_aliases(router: &RouterConfig, providers: &[Provider]) -> Result<()> {
    for (alias, target) in &router.model_aliases {
        anyhow::ensure!(
            !alias.trim().is_empty() && alias == alias.trim() && !alias.contains(','),
            "Router.modelAliases alias '{alias}' must be a nonblank bare model name without surrounding whitespace"
        );
        anyhow::ensure!(
            target == target.trim(),
            "Router.modelAliases target '{target}' has surrounding whitespace"
        );
        let Some((provider, model)) = target.split_once(',') else {
            anyhow::bail!(
                "Router.modelAliases target '{target}' must be an explicit provider,model route"
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
            "Router.modelAliases target '{target}' must name a configured provider,model route"
        );
    }
    Ok(())
}

/// Named routing preset with optional parameter overrides.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PresetConfig {
    /// Provider,model to route to (e.g., "anthropic,claude-sonnet-4-6")
    pub route: String,

    /// Optional max_tokens override
    #[serde(default)]
    pub max_tokens: Option<u32>,

    /// Optional temperature override
    #[serde(default)]
    pub temperature: Option<f32>,
}

/// Default maximum accepted request body size (64 MiB), applied to wire
/// bytes and to decoded content. Agent sessions with long histories exceed
/// the previous 10 MiB cap.
pub const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Hard ceiling for a configured `MAX_REQUEST_BODY_BYTES` (1 GiB).
pub const HARD_MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024 * 1024;

/// Parsed JSON configuration (deserializable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigFile {
    #[serde(rename = "Providers")]
    pub providers: Vec<Provider>,

    #[serde(rename = "Router")]
    pub router: RouterConfig,

    #[serde(default = "default_port")]
    #[serde(rename = "PORT")]
    pub port: u16,

    #[serde(default = "default_host")]
    #[serde(rename = "HOST")]
    pub host: String,

    #[serde(default = "default_timeout")]
    #[serde(rename = "API_TIMEOUT_MS")]
    pub api_timeout_ms: u64,

    #[serde(default)]
    #[serde(rename = "PROXY_URL")]
    pub proxy_url: Option<String>,

    /// Maximum number of idle connections per host in the shared HTTP pool.
    #[serde(default = "default_pool_max_idle_per_host")]
    #[serde(rename = "POOL_MAX_IDLE_PER_HOST")]
    pub pool_max_idle_per_host: usize,

    /// Idle connection timeout in milliseconds (0 = no timeout).
    #[serde(default = "default_pool_idle_timeout_ms")]
    #[serde(rename = "POOL_IDLE_TIMEOUT_MS")]
    pub pool_idle_timeout_ms: u64,

    /// SSE channel buffer size per stream (number of chunks).
    #[serde(default = "default_sse_buffer_size")]
    #[serde(rename = "SSE_BUFFER_SIZE")]
    pub sse_buffer_size: usize,

    /// Named preset configurations.
    #[serde(default)]
    #[serde(rename = "Presets")]
    pub presets: HashMap<String, PresetConfig>,

    /// Optional runtime persistence settings (for metrics/dashboard continuity).
    #[serde(default)]
    #[serde(rename = "Persistence")]
    pub persistence: PersistenceConfig,

    /// Debug capture settings (for recording raw API interactions).
    #[serde(default)]
    #[serde(rename = "DebugCapture")]
    pub debug_capture: DebugCaptureConfig,

    /// Optional Unix socket path for a local broker.
    /// When set, `with_broker_fallback` will attempt the broker first before
    /// falling back to a direct HTTP connection.
    /// Can also be set via the `CCR_BROKER_SOCKET` environment variable.
    #[serde(default)]
    #[serde(rename = "BROKER_SOCKET")]
    pub broker_socket: Option<String>,

    /// Maximum accepted request body size in bytes, enforced on the wire and
    /// after content decoding across all API endpoints. Large agent sessions
    /// routinely exceed tens of MiB. Zero falls back to the default and
    /// values above `HARD_MAX_REQUEST_BODY_BYTES` are clamped.
    #[serde(default = "default_max_request_body_bytes")]
    #[serde(rename = "MAX_REQUEST_BODY_BYTES")]
    pub max_request_body_bytes: usize,
}

/// Runtime configuration shared across all handlers via Axum state.
/// Wraps the parsed config plus a shared reqwest::Client connection pool.
#[derive(Debug, Clone)]
pub struct Config {
    inner: Arc<ConfigInner>,
    pub presets: HashMap<String, PresetConfig>,
}

#[derive(Debug)]
struct ConfigInner {
    file: ConfigFile,
    http_client: reqwest::Client,
}

impl Config {
    pub fn providers(&self) -> &[Provider] {
        &self.inner.file.providers
    }

    pub fn router(&self) -> &RouterConfig {
        &self.inner.file.router
    }

    #[allow(dead_code)]
    pub fn api_timeout_ms(&self) -> u64 {
        self.inner.file.api_timeout_ms
    }

    pub fn sse_buffer_size(&self) -> usize {
        self.inner.file.sse_buffer_size
    }

    /// Get the shared HTTP client. One pool for all requests.
    pub fn http_client(&self) -> &reqwest::Client {
        &self.inner.http_client
    }

    /// Get a preset by name.
    pub fn get_preset(&self, name: &str) -> Option<&PresetConfig> {
        self.presets.get(name)
    }

    /// Runtime persistence settings.
    pub fn persistence(&self) -> &PersistenceConfig {
        &self.inner.file.persistence
    }

    /// Debug capture settings.
    pub fn debug_capture(&self) -> &DebugCaptureConfig {
        &self.inner.file.debug_capture
    }

    /// Effective maximum request body size in bytes after defaulting and
    /// clamping.
    pub fn max_request_body_bytes(&self) -> usize {
        let requested = self.inner.file.max_request_body_bytes;
        if requested == 0 {
            DEFAULT_MAX_REQUEST_BODY_BYTES
        } else {
            requested.min(HARD_MAX_REQUEST_BODY_BYTES)
        }
    }

    /// Resolve the broker socket path.
    ///
    /// Priority: config file `BROKER_SOCKET` field > `CCR_BROKER_SOCKET` env var.
    pub fn broker_socket(&self) -> Option<String> {
        self.inner
            .file
            .broker_socket
            .clone()
            .or_else(|| std::env::var("CCR_BROKER_SOCKET").ok())
    }

    /// List all preset names.
    pub fn preset_names(&self) -> Vec<&str> {
        self.presets.keys().map(|s| s.as_str()).collect()
    }
}

impl Config {
    pub fn from_file(path: &str) -> Result<Self> {
        let raw_content =
            fs::read_to_string(path).context(format!("Failed to read config file: {}", path))?;
        // Parse the JSON first, then expand ${VAR} references per string value.
        // Expanding into the raw text before parsing breaks the document
        // whenever a substituted value contains JSON-hostile characters such
        // as a double quote (observed with a real MiniMax API key), which
        // surfaces as a misleading "expected `,` or `}`" parse error.
        let mut value: serde_json::Value =
            serde_json::from_str(&raw_content).context("Failed to parse config JSON")?;
        let mut expansion_failures: Vec<String> = Vec::new();
        expand_env_references(&mut value, &mut expansion_failures);
        if !expansion_failures.is_empty() {
            tracing::warn!(
                "Failed to expand env vars in config, using raw: {}",
                expansion_failures.join("; ")
            );
        }
        let file: ConfigFile =
            serde_json::from_value(value).context("Failed to parse config JSON")?;
        validate_provider_contracts(&file.providers)?;
        validate_model_aliases(&file.router, &file.providers)?;
        let allow_unexpanded_credentials = std::env::var("CCR_ALLOW_UNEXPANDED_CREDENTIALS")
            .is_ok_and(|value| value.eq_ignore_ascii_case("true"));
        validate_provider_credentials(&file, allow_unexpanded_credentials)?;

        // Build a single shared reqwest::Client with a properly-sized connection pool.
        let mut client_builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(file.api_timeout_ms))
            .pool_max_idle_per_host(file.pool_max_idle_per_host)
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .tcp_nodelay(true);

        if file.pool_idle_timeout_ms > 0 {
            client_builder = client_builder
                .pool_idle_timeout(std::time::Duration::from_millis(file.pool_idle_timeout_ms));
        }

        let http_client = client_builder.build()?;
        let presets = file.presets.clone();

        Ok(Config {
            inner: Arc::new(ConfigInner { file, http_client }),
            presets,
        })
    }

    /// Convert provider,model format to backend abbreviation.
    ///
    /// Returns the provider name portion for "provider,model" format,
    /// or the tier string as-is for simple tiers.
    ///
    /// For custom display names, configure `tier_name` in the provider config
    /// and use `backend_abbreviation_with_config()` instead.
    pub fn backend_abbreviation(tier: &str) -> String {
        if !tier.contains(',') {
            // Direct model name (codex, kimi, etc.)
            return tier.to_string();
        }
        // Return just the provider portion
        tier.split(',').next().unwrap_or(tier).to_string()
    }

    /// Convert provider,model format to backend abbreviation with config lookup.
    ///
    /// If the provider has `tier_name` configured, returns that.
    /// Otherwise falls back to the provider name.
    pub fn backend_abbreviation_with_config(&self, tier: &str) -> String {
        if !tier.contains(',') {
            return tier.to_string();
        }

        let provider_name = tier.split(',').next().unwrap_or(tier);

        // Look up provider config to get tier_name if configured
        if let Some(provider) = self.providers().iter().find(|p| p.name == provider_name) {
            if let Some(name) = provider.tier_name.as_ref() {
                return name.clone();
            }
        }

        provider_name.to_string()
    }

    /// Get backend tier order for fallback chain.
    pub fn backend_tiers(&self) -> Vec<String> {
        let r = self.router();

        // Prefer explicit tiers array if configured
        if let Some(ref tiers) = r.tiers {
            return tiers.clone();
        }

        // Fallback: build from individual fields
        let mut tiers = vec![r.default.clone()];

        for tier in [&r.background, &r.think].into_iter().flatten() {
            if !tiers.contains(tier) {
                tiers.push(tier.clone());
            }
        }

        tiers
    }

    pub fn resolve_provider(&self, model_route: &str) -> Option<&Provider> {
        let parts: Vec<&str> = model_route.split(',').collect();
        if parts.len() != 2 {
            return None;
        }

        let provider_name = parts[0];
        self.providers().iter().find(|p| p.name == provider_name)
    }

    /// Get retry config for a specific tier, falling back to defaults.
    pub fn get_tier_retry(&self, tier_name: &str) -> TierRetryConfig {
        self.router()
            .tier_retries
            .get(tier_name)
            .cloned()
            .unwrap_or_default()
    }
}

fn default_port() -> u16 {
    3456
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_timeout() -> u64 {
    600000 // 10 minutes
}

fn default_pool_max_idle_per_host() -> usize {
    64
}

fn default_pool_idle_timeout_ms() -> u64 {
    90000 // 90 seconds
}

fn default_sse_buffer_size() -> usize {
    32
}

fn default_max_request_body_bytes() -> usize {
    DEFAULT_MAX_REQUEST_BODY_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_request_body_bytes_defaults_zero_and_clamps() {
        let dir = std::env::temp_dir().join(format!("ccr-cfg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let base = r#"{"Providers": [], "Router": {"default": "unused"}}"#;

        std::fs::write(&path, base).unwrap();
        let config = Config::from_file(path.to_str().unwrap()).unwrap();
        assert_eq!(
            config.max_request_body_bytes(),
            DEFAULT_MAX_REQUEST_BODY_BYTES
        );

        std::fs::write(
            &path,
            r#"{"Providers": [], "Router": {"default": "unused"}, "MAX_REQUEST_BODY_BYTES": 1048576}"#,
        )
        .unwrap();
        let config = Config::from_file(path.to_str().unwrap()).unwrap();
        assert_eq!(config.max_request_body_bytes(), 1048576);

        std::fs::write(
            &path,
            r#"{"Providers": [], "Router": {"default": "unused"}, "MAX_REQUEST_BODY_BYTES": 0}"#,
        )
        .unwrap();
        let config = Config::from_file(path.to_str().unwrap()).unwrap();
        assert_eq!(
            config.max_request_body_bytes(),
            DEFAULT_MAX_REQUEST_BODY_BYTES
        );

        std::fs::write(
            &path,
            format!(
                r#"{{"Providers": [], "Router": {{"default": "unused"}}, "MAX_REQUEST_BODY_BYTES": {}}}"#,
                HARD_MAX_REQUEST_BODY_BYTES * 4
            ),
        )
        .unwrap();
        let config = Config::from_file(path.to_str().unwrap()).unwrap();
        assert_eq!(
            config.max_request_body_bytes(),
            HARD_MAX_REQUEST_BODY_BYTES
        );

        std::fs::remove_file(&path).unwrap();
    }

    /// Helper: parse a `ProviderTransformer` from a JSON string.
    fn parse_transformer(json: &str) -> ProviderTransformer {
        serde_json::from_str(json).expect("failed to parse ProviderTransformer")
    }

    /// Helper: parse a full `Provider` from a JSON string.
    fn parse_provider(json: &str) -> Provider {
        serde_json::from_str(json).expect("failed to parse Provider")
    }

    #[test]
    fn bare_string_use_list() {
        let t = parse_transformer(r#"{"use": ["deepseek", "openrouter"]}"#);
        assert_eq!(t.use_list.len(), 2);
        assert_eq!(t.use_list[0].name(), "deepseek");
        assert_eq!(t.use_list[1].name(), "openrouter");
        assert!(t.use_list[0].options().is_none());
        assert!(t.model_overrides.is_empty());
    }

    #[test]
    fn tuple_with_options() {
        let t =
            parse_transformer(r#"{"use": [["maxtoken", {"max_tokens": 65536}], "enhancetool"]}"#);
        assert_eq!(t.use_list.len(), 2);
        assert_eq!(t.use_list[0].name(), "maxtoken");
        let opts = t.use_list[0].options().unwrap();
        assert_eq!(opts["max_tokens"], 65536);
        assert_eq!(t.use_list[1].name(), "enhancetool");
        assert!(t.use_list[1].options().is_none());
    }

    #[test]
    fn model_specific_overrides() {
        let t = parse_transformer(
            r#"{
                "use": ["deepseek"],
                "deepseek-chat": {"use": ["tooluse"]}
            }"#,
        );
        assert_eq!(t.use_list.len(), 1);
        assert_eq!(t.use_list[0].name(), "deepseek");

        let chat = t.model_overrides.get("deepseek-chat").unwrap();
        assert_eq!(chat.use_list.len(), 1);
        assert_eq!(chat.use_list[0].name(), "tooluse");
    }

    #[test]
    fn complex_modelscope_pattern() {
        // Mirrors the real modelscope config from the Node.js README
        let t = parse_transformer(
            r#"{
                "use": [["maxtoken", {"max_tokens": 65536}], "enhancetool"],
                "Qwen/Qwen3-235B-A22B-Thinking-2507": {"use": ["reasoning"]}
            }"#,
        );
        assert_eq!(t.use_list.len(), 2);
        assert_eq!(t.use_list[0].name(), "maxtoken");
        assert_eq!(t.use_list[0].options().unwrap()["max_tokens"], 65536);
        assert_eq!(t.use_list[1].name(), "enhancetool");

        let qwen = t
            .model_overrides
            .get("Qwen/Qwen3-235B-A22B-Thinking-2507")
            .unwrap();
        assert_eq!(qwen.use_list.len(), 1);
        assert_eq!(qwen.use_list[0].name(), "reasoning");
    }

    #[test]
    fn provider_without_transformer() {
        let p = parse_provider(
            r#"{
                "name": "ollama",
                "api_base_url": "http://localhost:11434/v1/chat/completions",
                "api_key": "ollama",
                "models": ["qwen2.5-coder:latest"]
            }"#,
        );
        assert_eq!(p.protocol, ProviderProtocol::Openai);
        assert!(p.anthropic_version.is_none());
        assert!(p.transformer.is_none());
        assert!(p.pricing.is_none());
        assert!(p.model_pricing.is_empty());
        assert!(p.provider_transformers().is_empty());
        assert!(p.model_transformers("qwen2.5-coder:latest").is_none());
    }

    #[test]
    fn provider_pricing_parses_with_model_override() {
        let p = parse_provider(
            r#"{
                "name": "priced",
                "api_base_url": "https://example.test/v1",
                "api_key": "x",
                "models": ["economy", "premium"],
                "pricing": {
                    "input_per_million_tokens": 1.25,
                    "output_per_million_tokens": 5.0
                },
                "model_pricing": {
                    "premium": {
                        "input_per_million_tokens": 3.0,
                        "output_per_million_tokens": 15.0
                    }
                }
            }"#,
        );

        let economy = p.pricing_for_model("economy").expect("provider pricing");
        assert_eq!(economy.input_per_million_tokens, 1.25);
        assert_eq!(economy.output_per_million_tokens, 5.0);

        let premium = p.pricing_for_model("premium").expect("model pricing");
        assert_eq!(premium.input_per_million_tokens, 3.0);
        assert_eq!(premium.output_per_million_tokens, 15.0);
    }

    #[test]
    fn cached_rates_parse_and_discount_the_estimate() {
        let p = parse_provider(
            r#"{
                "name": "cached-priced",
                "api_base_url": "https://example.test/v1",
                "api_key": "x",
                "models": ["economy"],
                "pricing": {
                    "input_per_million_tokens": 3.0,
                    "output_per_million_tokens": 15.0,
                    "cache_read_per_million_tokens": 0.3,
                    "cache_creation_per_million_tokens": 3.75
                }
            }"#,
        );

        let pricing = p.pricing_for_model("economy").expect("provider pricing");
        assert_eq!(pricing.cache_read_per_million_tokens, Some(0.3));
        assert_eq!(pricing.cache_creation_per_million_tokens, Some(3.75));

        // 1M uncached * 3.0 + 100k out * 15.0 + 2M read * 0.3 + 400k created * 3.75
        // = 3.0 + 1.5 + 0.6 + 1.5 = 6.6 USD.
        let cost = pricing
            .estimate_request_cost_usd_with_cache(1_000_000, 2_000_000, 400_000, 100_000)
            .expect("valid pricing");
        assert!((cost - 6.6).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn unset_cached_rates_keep_estimates_a_lower_bound() {
        let p = parse_provider(
            r#"{
                "name": "base-priced",
                "api_base_url": "https://example.test/v1",
                "api_key": "x",
                "models": ["economy"],
                "pricing": {
                    "input_per_million_tokens": 1.0,
                    "output_per_million_tokens": 2.0
                }
            }"#,
        );

        let pricing = p.pricing_for_model("economy").expect("provider pricing");
        assert_eq!(pricing.cache_read_per_million_tokens, None);
        assert_eq!(pricing.cache_creation_per_million_tokens, None);

        // Cached tokens with no configured cached rate contribute nothing, so
        // the estimate equals the uncached-only bill (a lower bound), and the
        // legacy two-argument estimator matches the whole-prompt view.
        let with_cache = pricing
            .estimate_request_cost_usd_with_cache(500_000, 1_500_000, 250_000, 100_000)
            .expect("valid pricing");
        assert!((with_cache - 0.7).abs() < 1e-9, "got {with_cache}");
        let uncached_only = pricing
            .estimate_request_cost_usd(500_000, 100_000)
            .expect("valid pricing");
        assert!((with_cache - uncached_only).abs() < 1e-9);
    }

    #[test]
    fn invalid_cached_rate_is_ignored_but_base_pricing_survives() {
        let pricing = ModelPricing {
            input_per_million_tokens: 1.0,
            output_per_million_tokens: 2.0,
            cache_read_per_million_tokens: Some(-1.0),
            cache_creation_per_million_tokens: Some(f64::NAN),
        };

        let cost = pricing
            .estimate_request_cost_usd_with_cache(1_000_000, 9_000_000, 9_000_000, 500_000)
            .expect("base rates remain valid");
        assert!((cost - 2.0).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn provider_with_transformer() {
        let p = parse_provider(
            r#"{
                "name": "deepseek",
                "api_base_url": "https://api.deepseek.com/chat/completions",
                "api_key": "sk-xxx",
                "models": ["deepseek-chat", "deepseek-reasoner"],
                "transformer": {
                    "use": ["deepseek"],
                    "deepseek-chat": {"use": ["tooluse"]}
                }
            }"#,
        );
        assert_eq!(p.provider_transformers().len(), 1);
        assert_eq!(p.provider_transformers()[0].name(), "deepseek");

        let chat = p.model_transformers("deepseek-chat").unwrap();
        assert_eq!(chat.len(), 1);
        assert_eq!(chat[0].name(), "tooluse");

        assert!(p.model_transformers("deepseek-reasoner").is_none());
    }

    #[test]
    fn provider_with_anthropic_protocol() {
        let p = parse_provider(
            r#"{
                "name": "generic-anthropic",
                "api_base_url": "https://api.example.com/anthropic/v1",
                "api_key": "mk-xxx",
                "models": ["model-v1"],
                "protocol": "anthropic",
                "anthropic_version": "2023-06-01",
                "auth_header": "authorization"
            }"#,
        );
        assert_eq!(p.protocol, ProviderProtocol::Anthropic);
        assert_eq!(p.anthropic_version.as_deref(), Some("2023-06-01"));
        assert_eq!(p.auth_header.as_deref(), Some("authorization"));
    }

    #[test]
    fn provider_with_responses_protocol() {
        let p = parse_provider(
            r#"{
                "name": "meta-muse",
                "api_base_url": "https://api.meta.ai/v1",
                "api_key": "meta-test",
                "models": ["muse-spark-1.1"],
                "protocol": "responses"
            }"#,
        );
        assert_eq!(p.protocol, ProviderProtocol::Responses);
    }

    #[test]
    fn provider_with_forced_reasoning_effort() {
        let p = parse_provider(
            r#"{
                "name": "upstage",
                "api_base_url": "https://api.upstage.ai/v1",
                "api_key": "up-test",
                "models": ["solar-pro4"],
                "force_reasoning_effort": "max"
            }"#,
        );

        assert_eq!(p.force_reasoning_effort.as_deref(), Some("max"));
        validate_provider_contracts(&[p]).unwrap();
    }

    #[test]
    fn forced_reasoning_effort_requires_openai_protocol() {
        let p = parse_provider(
            r#"{
                "name": "bad-responses",
                "api_base_url": "https://api.example.test/v1",
                "api_key": "test",
                "models": ["model"],
                "protocol": "responses",
                "force_reasoning_effort": "max"
            }"#,
        );

        let error = validate_provider_contracts(&[p]).unwrap_err();

        assert!(error.to_string().contains("requires protocol 'openai'"));
    }

    #[test]
    fn forced_reasoning_effort_rejects_unknown_values() {
        let p = parse_provider(
            r#"{
                "name": "bad-effort",
                "api_base_url": "https://api.example.test/v1",
                "api_key": "test",
                "models": ["model"],
                "force_reasoning_effort": "maximum"
            }"#,
        );

        let error = validate_provider_contracts(&[p]).unwrap_err();

        assert!(error.to_string().contains("invalid force_reasoning_effort"));
    }

    #[test]
    fn should_bypass_logic() {
        let t = parse_transformer(r#"{"use": ["anthropic"]}"#);
        assert!(t.should_bypass("anthropic", "some-model"));
        assert!(!t.should_bypass("openai", "some-model"));

        // With a model override that matches
        let t2 = parse_transformer(r#"{"use": ["anthropic"], "model-a": {"use": ["anthropic"]}}"#);
        assert!(t2.should_bypass("anthropic", "model-a"));

        // With a model override that doesn't match
        let t3 = parse_transformer(r#"{"use": ["anthropic"], "model-a": {"use": ["tooluse"]}}"#);
        assert!(!t3.should_bypass("anthropic", "model-a"));
        // Unknown model still bypasses (no override present)
        assert!(t3.should_bypass("anthropic", "model-b"));
    }

    #[test]
    fn empty_transformer_object() {
        let t = parse_transformer(r#"{}"#);
        assert!(t.use_list.is_empty());
        assert!(t.model_overrides.is_empty());
        assert!(t.is_empty());
    }

    #[test]
    fn tuple_with_no_options_defaults_to_empty_object() {
        let t = parse_transformer(r#"{"use": [["myname"]]}"#);
        assert_eq!(t.use_list.len(), 1);
        assert_eq!(t.use_list[0].name(), "myname");
        let opts = t.use_list[0].options().unwrap();
        assert!(opts.is_object());
        assert_eq!(opts.as_object().unwrap().len(), 0);
    }

    #[test]
    fn full_config_roundtrip() {
        // Parse a complete ConfigFile with multiple providers exercising all
        // transformer patterns, then verify it round-trips through serde.
        let json = r#"{
            "Providers": [
                {
                    "name": "openrouter",
                    "api_base_url": "https://openrouter.ai/api/v1/chat/completions",
                    "api_key": "sk-xxx",
                    "models": ["google/gemini-3.1-pro-preview"],
                    "transformer": {"use": ["openrouter"]}
                },
                {
                    "name": "deepseek",
                    "api_base_url": "https://api.deepseek.com/chat/completions",
                    "api_key": "sk-xxx",
                    "models": ["deepseek-chat", "deepseek-reasoner"],
                    "transformer": {
                        "use": ["deepseek"],
                        "deepseek-chat": {"use": ["tooluse"]}
                    }
                },
                {
                    "name": "ollama",
                    "api_base_url": "http://localhost:11434/v1/chat/completions",
                    "api_key": "ollama",
                    "models": ["qwen2.5-coder:latest"]
                },
                {
                    "name": "modelscope",
                    "api_base_url": "https://api-inference.modelscope.cn/v1/chat/completions",
                    "api_key": "",
                    "models": ["Qwen/Qwen3-Coder-480B"],
                    "transformer": {
                        "use": [["maxtoken", {"max_tokens": 65536}], "enhancetool"],
                        "Qwen/Qwen3-235B-A22B-Thinking-2507": {"use": ["reasoning"]}
                    }
                }
            ],
            "Router": {
                "default": "deepseek,deepseek-chat"
            }
        }"#;

        let config: ConfigFile = serde_json::from_str(json).expect("parse ConfigFile");
        assert_eq!(config.providers.len(), 4);

        // openrouter
        let or = &config.providers[0];
        assert_eq!(or.provider_transformers().len(), 1);
        assert_eq!(or.provider_transformers()[0].name(), "openrouter");

        // deepseek with model override
        let ds = &config.providers[1];
        assert_eq!(ds.provider_transformers()[0].name(), "deepseek");
        let chat = ds.model_transformers("deepseek-chat").unwrap();
        assert_eq!(chat[0].name(), "tooluse");

        // ollama without transformer
        assert!(config.providers[2].transformer.is_none());

        // modelscope with tuple + model override
        let ms = &config.providers[3];
        assert_eq!(ms.provider_transformers().len(), 2);
        assert_eq!(ms.provider_transformers()[0].name(), "maxtoken");
        assert_eq!(ms.provider_transformers()[1].name(), "enhancetool");

        // Verify serialization round-trips
        let serialized = serde_json::to_string(&config).expect("serialize");
        assert!(serialized.contains("maxtoken"));
        assert!(serialized.contains("enhancetool"));
    }

    #[test]
    fn persistence_defaults_to_none() {
        let config: ConfigFile = serde_json::from_str(
            r#"{
                "Providers": [{
                    "name": "mock",
                    "api_base_url": "http://localhost:9999",
                    "api_key": "x",
                    "models": ["m"]
                }],
                "Router": {"default": "mock,m"}
            }"#,
        )
        .expect("parse ConfigFile");

        assert_eq!(config.persistence.mode, PersistenceMode::None);
        assert!(config.persistence.redis_url.is_none());
        assert_eq!(config.persistence.redis_prefix, "ccr-rust:persistence:v1");
    }

    #[test]
    fn persistence_redis_parses() {
        let config: ConfigFile = serde_json::from_str(
            r#"{
                "Providers": [{
                    "name": "mock",
                    "api_base_url": "http://localhost:9999",
                    "api_key": "x",
                    "models": ["m"]
                }],
                "Router": {"default": "mock,m"},
                "Persistence": {
                    "mode": "redis",
                    "redis_url": "redis://127.0.0.1:6379/0",
                    "redis_prefix": "ccr:test"
                }
            }"#,
        )
        .expect("parse ConfigFile");

        assert_eq!(config.persistence.mode, PersistenceMode::Redis);
        assert_eq!(
            config.persistence.redis_url.as_deref(),
            Some("redis://127.0.0.1:6379/0")
        );
        assert_eq!(config.persistence.redis_prefix, "ccr:test");
    }
}

#[cfg(test)]
mod credential_guard_tests {
    use super::*;

    fn file_with_api_key(api_key: &str) -> ConfigFile {
        serde_json::from_str(&format!(
            r#"{{"Providers": [{{"name": "p1", "api_base_url": "http://x", "api_key": "{api_key}", "models": ["m"]}}], "Router": {{"default": "p1,m"}}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn unexpanded_api_key_is_rejected() {
        let file = file_with_api_key("${CCR_DEFINITELY_MISSING_KEY}");
        let error = validate_provider_credentials(&file, false).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("provider 'p1' api_key"), "{message}");
        assert!(message.contains("CCR_ALLOW_UNEXPANDED_CREDENTIALS"), "{message}");
    }

    #[test]
    fn override_allows_unexpanded_keys() {
        let file = file_with_api_key("${CCR_DEFINITELY_MISSING_KEY}");
        validate_provider_credentials(&file, true).unwrap();
    }

    #[test]
    fn expanded_and_inline_keys_pass() {
        validate_provider_credentials(&file_with_api_key("real-key"), false).unwrap();
        validate_provider_credentials(&file_with_api_key(""), false).unwrap();
    }

    #[test]
    fn braceless_env_references_are_detected() {
        assert!(contains_env_placeholder("$CCR_MISSING_KEY"));
        assert!(contains_env_placeholder("prefix-${CCR_MISSING_KEY}"));
        assert!(contains_env_placeholder("bearer $AZURE_KEY extra"));
        assert!(!contains_env_placeholder("sk-real-key-123"));
        assert!(!contains_env_placeholder("cost $5 and $$ only"));
        assert!(!contains_env_placeholder(""));

        let file = file_with_api_key("$CCR_MISSING_KEY");
        let error = validate_provider_credentials(&file, false).unwrap_err();
        assert!(error.to_string().contains("provider 'p1' api_key"));
    }

    #[test]
    fn unexpanded_extra_header_is_rejected() {
        let raw = r#"{"Providers": [{"name": "p1", "api_base_url": "http://x", "api_key": "real", "models": ["m"], "extra_headers": {"api-key": "${CCR_AZURE_API_KEY}"}}], "Router": {"default": "p1,m"}}"#;
        let file: ConfigFile = serde_json::from_str(raw).unwrap();
        let error = validate_provider_credentials(&file, false).unwrap_err();
        assert!(error.to_string().contains("header 'api-key'"));
    }
}

#[cfg(test)]
mod env_expansion_tests {
    use super::*;

    #[test]
    fn quoted_env_value_expands_into_config_safely() {
        let dir = std::env::temp_dir().join(format!("ccr-cfg-quoted-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(
            &path,
            r#"{"Providers": [{"name": "p1", "api_base_url": "http://x", "api_key": "${CCR_TEST_QUOTED_KEY_9271}", "models": ["m"]}], "Router": {"default": "p1,m"}}"#,
        )
        .unwrap();
        // A value with JSON-hostile characters; textual pre-parse expansion
        // used to corrupt the document. Unique name; removed before asserts.
        std::env::set_var("CCR_TEST_QUOTED_KEY_9271", "sk-with\"quote-and\\backslash");
        let result = Config::from_file(path.to_str().unwrap());
        std::env::remove_var("CCR_TEST_QUOTED_KEY_9271");
        let config = result.expect("config with quoted env value must parse");
        assert_eq!(
            config.providers()[0].api_key,
            "sk-with\"quote-and\\backslash"
        );
        std::fs::remove_file(&path).unwrap();
    }
}
