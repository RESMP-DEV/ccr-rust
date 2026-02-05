use anyhow::{Context, Result};
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::sync::Arc;

/// Named routing preset with optional parameter overrides.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PresetConfig {
    /// Provider,model to route to (e.g., "anthropic,claude-3-opus")
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

    /// List all preset names.
    pub fn preset_names(&self) -> Vec<&str> {
        self.presets.keys().map(|s| s.as_str()).collect()
    }
}

/// A single entry in a transformer `use` array.
///
/// In the Node.js config this is either a bare string `"deepseek"` or a
/// tuple `["maxtoken", {"max_tokens": 65536}]`.
#[derive(Debug, Clone, Serialize)]
pub enum TransformerEntry {
    /// A transformer referenced by name with no options, e.g. `"deepseek"`.
    Name(String),
    /// A transformer with options, e.g. `["maxtoken", {"max_tokens": 65536}]`.
    WithOptions {
        name: String,
        options: serde_json::Value,
    },
}

impl TransformerEntry {
    pub fn name(&self) -> &str {
        match self {
            Self::Name(n) => n,
            Self::WithOptions { name, .. } => name,
        }
    }

    pub fn options(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Name(_) => None,
            Self::WithOptions { options, .. } => Some(options),
        }
    }
}

impl<'de> Deserialize<'de> for TransformerEntry {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct EntryVisitor;

        impl<'de> Visitor<'de> for EntryVisitor {
            type Value = TransformerEntry;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str(r#"a transformer name string or ["name", {options}] tuple"#)
            }

            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
                Ok(TransformerEntry::Name(v.to_owned()))
            }

            fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Self::Value, E> {
                Ok(TransformerEntry::Name(v))
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let name: String = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(0, &"a [name, options] tuple"))?;
                let options: serde_json::Value = seq
                    .next_element()?
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                Ok(TransformerEntry::WithOptions { name, options })
            }
        }

        deserializer.deserialize_any(EntryVisitor)
    }
}

/// A `use` list: an ordered sequence of transformer entries applied in order.
///
/// Corresponds to e.g. `"use": ["deepseek", ["maxtoken", {"max_tokens": 65536}]]`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[allow(dead_code)]
pub struct TransformerUseList {
    pub use_list: Vec<TransformerEntry>,
}

/// Model-specific transformer overrides.
///
/// In the Node.js config, any key in the `transformer` object that is *not*
/// `"use"` is treated as a model name whose value is an object containing its
/// own `"use"` array. For example:
///
/// ```json
/// "deepseek-chat": { "use": ["tooluse"] }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelTransformerConfig {
    #[serde(rename = "use", default)]
    pub use_list: Vec<TransformerEntry>,
}

/// Full nested transformer configuration for a provider.
///
/// Mirrors the Node.js config pattern where the `transformer` object contains:
///   - `"use"`: provider-level transformer chain (applied to all models)
///   - `"<model-name>"`: model-specific transformer chain overrides
///
/// Example:
/// ```json
/// {
///   "transformer": {
///     "use": ["deepseek"],
///     "deepseek-chat": { "use": ["tooluse"] }
///   }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Default)]
pub struct ProviderTransformer {
    /// Provider-level transformers applied to all models.
    pub use_list: Vec<TransformerEntry>,
    /// Model-specific transformer overrides keyed by model name.
    pub model_overrides: HashMap<String, ModelTransformerConfig>,
}

impl ProviderTransformer {
    /// Get the provider-level transformer chain.
    #[allow(dead_code)]
    // TODO: integrate into request flow or remove
    pub fn provider_transformers(&self) -> &[TransformerEntry] {
        &self.use_list
    }

    /// Get model-specific transformers, if any are configured for `model`.
    pub fn model_transformers(&self, model: &str) -> Option<&[TransformerEntry]> {
        self.model_overrides
            .get(model)
            .map(|m| m.use_list.as_slice())
    }

    /// Check whether this provider has any transformers configured at all.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.use_list.is_empty() && self.model_overrides.is_empty()
    }

    /// Check whether there is exactly one provider-level transformer and it
    /// matches `name`, with no model overrides. Used by the bypass/passthrough
    /// logic that mirrors the Node.js `shouldBypassTransformers`.
    #[allow(dead_code)]
    // TODO: integrate into request flow or remove
    pub fn is_sole_transformer(&self, name: &str) -> bool {
        self.use_list.len() == 1
            && self.use_list[0].name() == name
            && self.model_overrides.is_empty()
    }

    /// Check bypass eligibility for a specific model, matching the Node.js
    /// logic: provider has exactly one transformer matching `name`, and the
    /// model either has no overrides or a single override also matching `name`.
    #[allow(dead_code)]
    pub fn should_bypass(&self, transformer_name: &str, model: &str) -> bool {
        if self.use_list.len() != 1 || self.use_list[0].name() != transformer_name {
            return false;
        }
        match self.model_overrides.get(model) {
            None => true,
            Some(m) if m.use_list.is_empty() => true,
            Some(m) => m.use_list.len() == 1 && m.use_list[0].name() == transformer_name,
        }
    }
}

impl<'de> Deserialize<'de> for ProviderTransformer {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Deserialize the whole object as a generic JSON map first, then pull
        // out "use" separately from model-keyed overrides.
        let map: serde_json::Map<String, serde_json::Value> =
            serde_json::Map::deserialize(deserializer)?;

        let mut use_list = Vec::new();
        let mut model_overrides = HashMap::new();

        for (key, value) in map {
            if key == "use" {
                // Provider-level use list
                use_list = serde_json::from_value(value).map_err(de::Error::custom)?;
            } else {
                // Model-specific override
                let model_config: ModelTransformerConfig =
                    serde_json::from_value(value).map_err(de::Error::custom)?;
                model_overrides.insert(key, model_config);
            }
        }

        Ok(ProviderTransformer {
            use_list,
            model_overrides,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub name: String,
    pub api_base_url: String,
    pub api_key: String,
    pub models: Vec<String>,

    #[serde(default)]
    pub transformer: Option<ProviderTransformer>,
}

impl Provider {
    /// Get the provider-level transformer chain, or an empty slice if none.
    pub fn provider_transformers(&self) -> &[TransformerEntry] {
        self.transformer
            .as_ref()
            .map(|t| t.use_list.as_slice())
            .unwrap_or(&[])
    }

    /// Get model-specific transformers, if configured.
    pub fn model_transformers(&self, model: &str) -> Option<&[TransformerEntry]> {
        self.transformer
            .as_ref()
            .and_then(|t| t.model_transformers(model))
    }
}

/// Configuration for web search routing.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WebSearchConfig {
    /// Enable [search] tag detection
    #[serde(default)]
    pub enabled: bool,
    /// Provider,model for search-enabled requests
    pub search_provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterConfig {
    pub default: String,

    #[serde(default)]
    pub background: Option<String>,

    #[serde(default)]
    pub think: Option<String>,

    #[serde(default)]
    #[serde(rename = "longContext")]
    pub long_context: Option<String>,

    #[serde(default = "default_long_context_threshold")]
    #[serde(rename = "longContextThreshold")]
    pub long_context_threshold: u32,

    #[serde(default)]
    #[serde(rename = "webSearch")]
    pub web_search: WebSearchConfig,

    /// Per-tier retry configuration. Keys are tier names ("tier-0", "tier-1", etc.).
    /// If absent, all tiers use the global default (3 retries, 100ms base backoff).
    #[serde(default)]
    #[serde(rename = "tierRetries")]
    pub tier_retries: HashMap<String, TierRetryConfig>,

    /// Named presets that override model parameters and routing.
    #[serde(default)]
    #[serde(rename = "presets")]
    pub presets: HashMap<String, PresetConfig>,
}

/// Per-tier retry limits and backoff configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierRetryConfig {
    /// Maximum number of retry attempts for this tier.
    #[serde(default = "default_max_retries")]
    pub max_retries: usize,

    /// Base backoff delay in milliseconds.
    #[serde(default = "default_base_backoff_ms")]
    pub base_backoff_ms: u64,

    /// Backoff multiplier per attempt.
    #[serde(default = "default_backoff_multiplier")]
    pub backoff_multiplier: f64,

    /// Maximum backoff delay in milliseconds.
    #[serde(default = "default_max_backoff_ms")]
    pub max_backoff_ms: u64,
}

impl Config {
    pub fn from_file(path: &str) -> Result<Self> {
        let content =
            fs::read_to_string(path).context(format!("Failed to read config file: {}", path))?;
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

    /// Get backend tier order for fallback chain.
    pub fn backend_tiers(&self) -> Vec<String> {
        let r = self.router();
        let mut tiers = vec![r.default.clone()];

        for tier in [&r.background, &r.think, &r.long_context]
            .into_iter()
            .flatten()
        {
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

impl Default for TierRetryConfig {
    fn default() -> Self {
        Self {
            max_retries: default_max_retries(),
            base_backoff_ms: default_base_backoff_ms(),
            backoff_multiplier: default_backoff_multiplier(),
            max_backoff_ms: default_max_backoff_ms(),
        }
    }
}

impl TierRetryConfig {
    /// Calculate backoff duration for a given attempt (0-indexed).
    #[allow(dead_code)]
    pub fn backoff_duration(&self, attempt: usize) -> std::time::Duration {
        let delay_ms = (self.base_backoff_ms as f64) * self.backoff_multiplier.powi(attempt as i32);
        let clamped_ms = delay_ms.min(self.max_backoff_ms as f64) as u64;
        std::time::Duration::from_millis(clamped_ms)
    }

    /// Calculate backoff duration scaled by the tier's EWMA latency.
    ///
    /// The backoff is dynamically adjusted based on the ratio of the tier's
    /// observed latency to a reference baseline. This allows faster tiers to
    /// retry more aggressively while slowing down retries for degraded tiers.
    ///
    /// # Formula
    /// ```text
    /// scaled_backoff = base_backoff * multiplier^attempt * latency_factor
    ///
    /// latency_factor = max(0.5, min(3.0, ewma / baseline_ewma))
    ///
    /// where baseline_ewma = 1.0 second (configurable via reference_latency_ms)
    /// ```
    ///
    /// # Arguments
    /// * `attempt` - Retry attempt index (0-indexed)
    /// * `ewma_secs` - The tier's current EWMA latency in seconds, if available
    ///
    /// # Returns
    /// The scaled backoff duration, clamped to `[base_backoff_ms, max_backoff_ms]`
    pub fn backoff_duration_with_ewma(
        &self,
        attempt: usize,
        ewma_secs: Option<f64>,
    ) -> std::time::Duration {
        // Base exponential backoff
        let base_delay_ms =
            (self.base_backoff_ms as f64) * self.backoff_multiplier.powi(attempt as i32);

        // Apply latency scaling if we have EWMA data
        let scaled_delay_ms = match ewma_secs {
            Some(ewma) if ewma > 0.0 => {
                // Convert EWMA to milliseconds for comparison
                let ewma_ms = ewma * 1000.0;

                // Calculate scaling factor relative to a 1-second baseline
                // Faster tiers (<500ms) get 0.5x scaling
                // Normal tiers (~1s) get 1.0x scaling
                // Slower tiers (>2s) get up to 3.0x scaling
                let reference_ms = 1000.0; // 1-second baseline
                let latency_factor = (ewma_ms / reference_ms).clamp(0.5, 3.0);

                base_delay_ms * latency_factor
            }
            _ => base_delay_ms,
        };

        // Clamp to max backoff, and floor at 0.5x of base (fast tier minimum)
        let clamped_ms = scaled_delay_ms.min(self.max_backoff_ms as f64);
        let min_backoff = self.base_backoff_ms as f64 * 0.5;

        std::time::Duration::from_millis(clamped_ms.max(min_backoff) as u64)
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

fn default_long_context_threshold() -> u32 {
    1048576
}

fn default_max_retries() -> usize {
    3
}

fn default_base_backoff_ms() -> u64 {
    100
}

fn default_backoff_multiplier() -> f64 {
    2.0
}

fn default_max_backoff_ms() -> u64 {
    10000
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

/// Tests for EWMA-aware backoff scaling.
#[cfg(test)]
mod backoff_tests {
    use super::*;

    fn default_retry_config() -> TierRetryConfig {
        TierRetryConfig::default()
    }

    #[test]
    fn ewma_backoff_no_ewma_uses_base() {
        let config = default_retry_config();
        let duration = config.backoff_duration_with_ewma(0, None);
        // Without EWMA, should fall back to standard exponential backoff
        assert_eq!(duration.as_millis(), 100);
    }

    #[test]
    fn ewma_backoff_fast_tier_scales_down() {
        let config = default_retry_config();
        // Fast tier: 300ms EWMA (should scale down by 0.5x)
        let duration = config.backoff_duration_with_ewma(0, Some(0.3));
        assert_eq!(duration.as_millis(), 50); // 100ms * 0.5
    }

    #[test]
    fn ewma_backoff_normal_tier_no_scaling() {
        let config = default_retry_config();
        // Normal tier: 1.0s EWMA (baseline, 1.0x scaling)
        let duration = config.backoff_duration_with_ewma(0, Some(1.0));
        assert_eq!(duration.as_millis(), 100); // 100ms * 1.0
    }

    #[test]
    fn ewma_backoff_slow_tier_scales_up() {
        let config = default_retry_config();
        // Slow tier: 2.5s EWMA (should scale up by 2.5x)
        let duration = config.backoff_duration_with_ewma(0, Some(2.5));
        assert_eq!(duration.as_millis(), 250); // 100ms * 2.5
    }

    #[test]
    fn ewma_backoff_very_slow_tier_clamped_to_max() {
        let config = default_retry_config();
        // Very slow tier: 5.0s EWMA (would be 5.0x, but clamped to 3.0x)
        let duration = config.backoff_duration_with_ewma(0, Some(5.0));
        assert_eq!(duration.as_millis(), 300); // 100ms * 3.0 (clamped)
    }

    #[test]
    fn ewma_backoff_respects_max_backoff() {
        let config = TierRetryConfig {
            max_retries: 5,
            base_backoff_ms: 100,
            backoff_multiplier: 2.0,
            max_backoff_ms: 500,
        };
        // Even with slow tier scaling, should clamp to max
        let duration = config.backoff_duration_with_ewma(2, Some(5.0));
        // Base: 100 * 2^2 = 400, scaled by 3.0 = 1200, clamped to 500
        assert_eq!(duration.as_millis(), 500);
    }

    #[test]
    fn ewma_backoff_exponential_growth_with_scaling() {
        let config = default_retry_config();
        // Tier with 2.0s EWMA (2.0x scaling)
        let d0 = config.backoff_duration_with_ewma(0, Some(2.0));
        let d1 = config.backoff_duration_with_ewma(1, Some(2.0));
        let d2 = config.backoff_duration_with_ewma(2, Some(2.0));

        // Attempt 0: 100ms * 1 * 2.0 = 200ms
        assert_eq!(d0.as_millis(), 200);
        // Attempt 1: 100ms * 2 * 2.0 = 400ms
        assert_eq!(d1.as_millis(), 400);
        // Attempt 2: 100ms * 4 * 2.0 = 800ms
        assert_eq!(d2.as_millis(), 800);
    }

    #[test]
    fn ewma_backoff_never_below_base() {
        let config = default_retry_config();
        // Very fast tier (0.1s EWMA) would scale to 0.1x, but floors at 0.5x base
        let duration = config.backoff_duration_with_ewma(0, Some(0.1));
        assert_eq!(duration.as_millis(), 50); // 50ms = 0.5 * base_backoff_ms (floor)
    }

    #[test]
    fn ewma_backoff_zero_ewma_uses_base() {
        let config = default_retry_config();
        // Zero EWMA shouldn't cause division or scaling issues
        let duration = config.backoff_duration_with_ewma(0, Some(0.0));
        assert_eq!(duration.as_millis(), 100); // Falls back to base
    }
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
        assert!(p.transformer.is_none());
        assert!(p.provider_transformers().is_empty());
        assert!(p.model_transformers("qwen2.5-coder:latest").is_none());
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
                    "models": ["google/gemini-2.5-pro-preview"],
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
}
