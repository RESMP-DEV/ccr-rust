use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub name: String,
    pub api_base_url: String,
    pub api_key: String,
    pub models: Vec<String>,
    
    #[serde(default)]
    pub transformer: Option<HashMap<String, serde_json::Value>>,
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
    pub web_search: Option<String>,

    /// Per-tier retry configuration. Keys are tier names ("tier-0", "tier-1", etc.).
    /// If absent, all tiers use the global default (3 retries, 100ms base backoff).
    #[serde(default)]
    #[serde(rename = "tierRetries")]
    pub tier_retries: HashMap<String, TierRetryConfig>,
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
        let content = fs::read_to_string(path)
            .context(format!("Failed to read config file: {}", path))?;
        let config: Config = serde_json::from_str(&content)
            .context("Failed to parse config JSON")?;
        Ok(config)
    }

    /// Get backend tier order for fallback chain
    pub fn backend_tiers(&self) -> Vec<String> {
        // Extract tier order from Router config
        let mut tiers = vec![self.router.default.clone()];
        
        // Add other configured routes as fallback tiers
        if let Some(bg) = &self.router.background {
            if !tiers.contains(bg) {
                tiers.push(bg.clone());
            }
        }
        if let Some(think) = &self.router.think {
            if !tiers.contains(think) {
                tiers.push(think.clone());
            }
        }
        if let Some(long) = &self.router.long_context {
            if !tiers.contains(long) {
                tiers.push(long.clone());
            }
        }
        
        tiers
    }

    pub fn resolve_provider(&self, model_route: &str) -> Option<&Provider> {
        // Parse "provider,model" format
        let parts: Vec<&str> = model_route.split(',').collect();
        if parts.len() != 2 {
            return None;
        }

        let provider_name = parts[0];
        self.providers.iter().find(|p| p.name == provider_name)
    }

    /// Get retry config for a specific tier, falling back to defaults.
    pub fn get_tier_retry(&self, tier_name: &str) -> TierRetryConfig {
        self.router
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
    pub fn backoff_duration(&self, attempt: usize) -> std::time::Duration {
        let delay_ms = (self.base_backoff_ms as f64)
            * self.backoff_multiplier.powi(attempt as i32);
        let clamped_ms = delay_ms.min(self.max_backoff_ms as f64) as u64;
        std::time::Duration::from_millis(clamped_ms)
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
    60000
}
