// SPDX-License-Identifier: AGPL-3.0-or-later
mod types;
pub use types::*;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::sync::Arc;

use crate::debug_capture::DebugCaptureConfig;

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
        // Expand ${VAR} env var references in config values (e.g., api_key: "${ZAI_API_KEY}")
        let content = shellexpand::env(&raw_content)
            .map(|s| s.into_owned())
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to expand env vars in config, using raw: {e}");
                raw_content.clone()
            });
        let file: ConfigFile =
            serde_json::from_str(&content).context("Failed to parse config JSON")?;

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

#[cfg(test)]
mod tests {
    use super::*;

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
