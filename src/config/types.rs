// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::fmt;

use super::PresetConfig;

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

    /// Upstream API protocol for this provider.
    ///
    /// - `openai` (default): send OpenAI-compatible `/chat/completions` requests.
    /// - `anthropic`: send Anthropic-compatible `/messages` requests.
    #[serde(default)]
    pub protocol: ProviderProtocol,

    /// Anthropic API version header for `protocol=anthropic`.
    ///
    /// Defaults to the canonical Anthropic-compatible version when omitted.
    #[serde(default)]
    pub anthropic_version: Option<String>,

    #[serde(default)]
    pub transformer: Option<ProviderTransformer>,

    /// Optional display name for metrics/dashboard (e.g., "ccr-glm").
    /// If not specified, defaults to the provider name.
    #[serde(default)]
    pub tier_name: Option<String>,

    /// Extra headers to include in requests to this provider.
    ///
    /// Useful for providers that require specific headers (e.g., `User-Agent`
    /// for Kimi's coding agent identity check).
    #[serde(default)]
    pub extra_headers: Option<std::collections::HashMap<String, String>>,

    /// When true (default), proactively skip this tier if upstream reports
    /// `X-RateLimit-Remaining: 0` on a successful response.  Set to `false`
    /// for providers like Z.AI that include rate-limit headers on every 200
    /// as informational warnings without actually rejecting at quota zero.
    #[serde(default = "default_honor_ratelimit_headers")]
    pub honor_ratelimit_headers: bool,

    /// When true, this provider streams natively even when the global
    /// `forceNonStreaming` is enabled.  Useful for providers where
    /// non-streaming mode causes excessive latency (e.g. Gemini).
    #[serde(default)]
    pub allow_streaming: bool,
}

fn default_honor_ratelimit_headers() -> bool {
    true
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

/// Provider upstream API protocol.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProviderProtocol {
    #[default]
    Openai,
    Anthropic,
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

/// Acquisition strategies for the experimental GP reranker.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum GpAcquisitionStrategy {
    #[default]
    Ucb,
    Thompson,
    Greedy,
    EpsilonGreedy,
}

/// Runtime settings for the optional GP-based tier reranker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpRoutingRuntimeConfig {
    /// Enable GP reranking on top of EWMA ordering.
    #[serde(default)]
    pub enabled: bool,

    /// Maximum number of configured tiers to expose to the GP.
    #[serde(default = "default_gp_max_candidates")]
    pub max_candidates: usize,

    /// Number of observations retained in the in-memory training buffer.
    #[serde(default = "default_gp_buffer_capacity")]
    pub buffer_capacity: usize,

    /// Minimum observations before the surrogate is fit.
    #[serde(default = "default_gp_min_observations")]
    pub min_observations: usize,

    /// Number of new observations between automatic refits. `0` disables
    /// periodic refits after the initial fit.
    #[serde(default = "default_gp_refit_interval")]
    pub refit_interval: usize,

    /// UCB exploration weight when `acquisition = "ucb"`.
    #[serde(default = "default_gp_ucb_kappa")]
    pub ucb_kappa: f32,

    /// Nugget (jitter) passed to the GP model.
    #[serde(default = "default_gp_nugget")]
    pub nugget: f32,

    /// Optional KPLS dimensionality reduction for the surrogate.
    #[serde(default)]
    pub kpls_dim: Option<usize>,

    /// Prior mean used before a fitted model is available.
    #[serde(default = "default_gp_prior_mean")]
    pub prior_mean: f32,

    /// Prior variance used before a fitted model is available.
    #[serde(default = "default_gp_prior_variance")]
    pub prior_variance: f32,

    /// Acquisition strategy used for reranking.
    #[serde(default)]
    pub acquisition: GpAcquisitionStrategy,

    /// Random exploration rate when `acquisition = "epsilonGreedy"`.
    #[serde(default = "default_gp_epsilon")]
    pub epsilon: f32,
}

impl Default for GpRoutingRuntimeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_candidates: default_gp_max_candidates(),
            buffer_capacity: default_gp_buffer_capacity(),
            min_observations: default_gp_min_observations(),
            refit_interval: default_gp_refit_interval(),
            ucb_kappa: default_gp_ucb_kappa(),
            nugget: default_gp_nugget(),
            kpls_dim: None,
            prior_mean: default_gp_prior_mean(),
            prior_variance: default_gp_prior_variance(),
            acquisition: GpAcquisitionStrategy::default(),
            epsilon: default_gp_epsilon(),
        }
    }
}

/// Persistence backend mode.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PersistenceMode {
    #[default]
    None,
    Redis,
}

/// Runtime persistence configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistenceConfig {
    /// Persistence backend mode.
    #[serde(default)]
    pub mode: PersistenceMode,

    /// Redis URL used when mode = `redis`.
    ///
    /// Example: `redis://127.0.0.1:6379/0`
    #[serde(default)]
    pub redis_url: Option<String>,

    /// Prefix for Redis keys used by CCR-Rust persistence.
    #[serde(default = "default_redis_prefix")]
    pub redis_prefix: String,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            mode: PersistenceMode::None,
            redis_url: None,
            redis_prefix: default_redis_prefix(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterConfig {
    pub default: String,

    #[serde(default)]
    pub background: Option<String>,

    #[serde(default)]
    pub think: Option<String>,

    /// Force all requests to use non-streaming mode.
    /// Useful for agent workloads where SSE frame limits cause parsing errors.
    /// Default: false (preserve client's stream preference).
    #[serde(default)]
    #[serde(rename = "forceNonStreaming")]
    pub force_non_streaming: bool,

    /// Ignore direct model routing requests (e.g., "openrouter,model").
    /// When true, always use tier order instead of honoring client's explicit model.
    /// Default: false (client can directly target specific backends).
    #[serde(default)]
    #[serde(rename = "ignoreDirect")]
    pub ignore_direct: bool,

    /// Explicit tier ordering for cascading fallback.
    /// If present, overrides automatic tier construction from default/background/think.
    #[serde(default)]
    pub tiers: Option<Vec<String>>,

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

    /// Request batching configuration.
    #[serde(default)]
    #[serde(rename = "batching")]
    pub batching: Option<BatchingConfig>,

    /// Number of top-performing providers to consider for routing.
    #[serde(default)]
    #[serde(rename = "topK")]
    pub top_k: Option<usize>,

    /// Temperature for softmax routing. Higher values flatten the distribution.
    #[serde(default)]
    #[serde(rename = "routingTemperature")]
    pub routing_temperature: Option<f64>,

    /// Optional GP reranker layered on top of the EWMA ordering.
    #[serde(default)]
    #[serde(rename = "gpRouting")]
    pub gp_routing: GpRoutingRuntimeConfig,
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

    /// Maximum time to wait for the first useful SSE frame after upstream headers arrive.
    #[serde(
        default = "default_stream_first_event_timeout_ms",
        alias = "streamFirstEventTimeoutMs"
    )]
    pub stream_first_event_timeout_ms: u64,

    /// Maximum idle time between useful SSE frames once streaming has started.
    #[serde(
        default = "default_stream_idle_timeout_ms",
        alias = "streamIdleTimeoutMs"
    )]
    pub stream_idle_timeout_ms: u64,
}

/// Request batching configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchingConfig {
    /// Maximum number of requests to buffer before sending a batch.
    #[serde(default = "default_batching_max_requests")]
    pub max_requests: usize,

    /// Maximum time in milliseconds to wait before sending a batch, even if not full.
    #[serde(default = "default_batching_max_latency_ms")]
    pub max_latency_ms: u64,
}

impl Default for TierRetryConfig {
    fn default() -> Self {
        Self {
            max_retries: default_max_retries(),
            base_backoff_ms: default_base_backoff_ms(),
            backoff_multiplier: default_backoff_multiplier(),
            max_backoff_ms: default_max_backoff_ms(),
            stream_first_event_timeout_ms: default_stream_first_event_timeout_ms(),
            stream_idle_timeout_ms: default_stream_idle_timeout_ms(),
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

    pub fn stream_first_event_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.stream_first_event_timeout_ms)
    }

    pub fn stream_idle_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.stream_idle_timeout_ms)
    }
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

fn default_stream_first_event_timeout_ms() -> u64 {
    120000
}

fn default_stream_idle_timeout_ms() -> u64 {
    120000
}

fn default_batching_max_requests() -> usize {
    16
}

fn default_batching_max_latency_ms() -> u64 {
    100
}

fn default_redis_prefix() -> String {
    "ccr-rust:persistence:v1".to_string()
}

fn default_gp_max_candidates() -> usize {
    8
}

fn default_gp_buffer_capacity() -> usize {
    500
}

fn default_gp_min_observations() -> usize {
    40
}

fn default_gp_refit_interval() -> usize {
    200
}

fn default_gp_ucb_kappa() -> f32 {
    2.0
}

fn default_gp_nugget() -> f32 {
    1e-4
}

fn default_gp_prior_mean() -> f32 {
    0.5
}

fn default_gp_prior_variance() -> f32 {
    0.25
}

fn default_gp_epsilon() -> f32 {
    0.1
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
            stream_first_event_timeout_ms: default_stream_first_event_timeout_ms(),
            stream_idle_timeout_ms: default_stream_idle_timeout_ms(),
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

    #[test]
    fn gp_routing_defaults_disabled() {
        let router: RouterConfig = serde_json::from_str(
            r#"{
            "default": "mock,m"
        }"#,
        )
        .expect("parse RouterConfig");

        assert!(!router.gp_routing.enabled);
        assert_eq!(router.gp_routing.max_candidates, 8);
        assert_eq!(router.gp_routing.acquisition, GpAcquisitionStrategy::Ucb);
    }

    #[test]
    fn gp_routing_parses_custom_values() {
        let router: RouterConfig = serde_json::from_str(
            r#"{
            "default": "mock,m",
            "gpRouting": {
                "enabled": true,
                "maxCandidates": 6,
                "minObservations": 12,
                "refitInterval": 50,
                "ucbKappa": 1.25,
                "acquisition": "thompson"
            }
        }"#,
        )
        .expect("parse RouterConfig");

        assert!(router.gp_routing.enabled);
        assert_eq!(router.gp_routing.max_candidates, 6);
        assert_eq!(router.gp_routing.min_observations, 12);
        assert_eq!(router.gp_routing.refit_interval, 50);
        assert!((router.gp_routing.ucb_kappa - 1.25).abs() < f32::EPSILON);
        assert_eq!(
            router.gp_routing.acquisition,
            GpAcquisitionStrategy::Thompson
        );
    }
}
