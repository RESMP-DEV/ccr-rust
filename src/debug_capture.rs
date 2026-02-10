//! Debug capture module for recording raw request/response data.
//!
//! This module captures raw API interactions for debugging provider issues,
//! particularly useful for observing drift in provider responses over time.
//!
//! # Example Configuration
//!
//! ```json
//! {
//!   "DebugCapture": {
//!     "enabled": true,
//!     "providers": ["minimax"],
//!     "output_dir": "~/.ccr-rust/captures",
//!     "max_files": 1000,
//!     "include_headers": false
//!   }
//! }
//! ```
//!
//! Captured files are stored as JSON with timestamped filenames:
//! `{provider}_{timestamp}_{request_id}.json`

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Global request counter for unique IDs.
static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Configuration for debug capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugCaptureConfig {
    /// Enable debug capture globally.
    #[serde(default)]
    pub enabled: bool,

    /// List of provider names to capture (e.g., ["minimax", "deepseek"]).
    /// Empty list means capture all providers when enabled.
    #[serde(default)]
    pub providers: Vec<String>,

    /// Output directory for capture files. Supports ~ expansion.
    #[serde(default = "default_output_dir")]
    pub output_dir: String,

    /// Maximum number of capture files to keep (oldest are deleted).
    #[serde(default = "default_max_files")]
    pub max_files: usize,

    /// Include raw HTTP headers in capture.
    #[serde(default)]
    pub include_headers: bool,

    /// Capture response body even on success (normally only captures on error).
    #[serde(default = "default_capture_success")]
    pub capture_success: bool,

    /// Maximum response body size to capture (bytes). 0 = unlimited.
    #[serde(default = "default_max_body_size")]
    pub max_body_size: usize,
}

impl Default for DebugCaptureConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            providers: vec![],
            output_dir: default_output_dir(),
            max_files: default_max_files(),
            include_headers: false,
            capture_success: true,
            max_body_size: default_max_body_size(),
        }
    }
}

fn default_output_dir() -> String {
    "~/.ccr-rust/captures".to_string()
}

fn default_max_files() -> usize {
    1000
}

fn default_capture_success() -> bool {
    true
}

fn default_max_body_size() -> usize {
    1024 * 1024 // 1MB default
}

/// Captured request/response pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedInteraction {
    /// Unique request ID.
    pub request_id: u64,

    /// Provider name (e.g., "minimax").
    pub provider: String,

    /// Tier name for display (e.g., "ccr-mm").
    pub tier_name: String,

    /// Model name used.
    pub model: String,

    /// Timestamp of capture (ISO 8601).
    pub timestamp: String,

    /// Request URL.
    pub url: String,

    /// Request method (POST, GET, etc.).
    pub method: String,

    /// Request headers (if include_headers is true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_headers: Option<serde_json::Value>,

    /// Request body as JSON.
    pub request_body: serde_json::Value,

    /// Response status code.
    pub response_status: u16,

    /// Response headers (if include_headers is true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_headers: Option<serde_json::Value>,

    /// Response body (raw string, may be truncated).
    pub response_body: String,

    /// Whether response was truncated due to max_body_size.
    pub response_truncated: bool,

    /// Response latency in milliseconds.
    pub latency_ms: u64,

    /// Whether this was a streaming response.
    pub is_streaming: bool,

    /// Whether the request succeeded (2xx status).
    pub success: bool,

    /// Error message if request failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// Additional metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Debug capture manager.
#[derive(Debug)]
pub struct DebugCapture {
    config: DebugCaptureConfig,
    output_path: PathBuf,
    provider_filter: HashSet<String>,
    file_count: Arc<RwLock<usize>>,
}

impl DebugCapture {
    /// Create a new debug capture manager.
    pub fn new(config: DebugCaptureConfig) -> Result<Self> {
        let output_path = expand_tilde(&config.output_dir);

        // Create output directory if it doesn't exist
        if config.enabled {
            fs::create_dir_all(&output_path)?;
            info!(
                "Debug capture enabled: {} (providers: {:?})",
                output_path.display(),
                if config.providers.is_empty() {
                    vec!["*".to_string()]
                } else {
                    config.providers.clone()
                }
            );
        }

        let provider_filter: HashSet<String> =
            config.providers.iter().map(|s| s.to_lowercase()).collect();

        Ok(Self {
            config,
            output_path,
            provider_filter,
            file_count: Arc::new(RwLock::new(0)),
        })
    }

    /// Check if capture is enabled for a given provider.
    pub fn should_capture(&self, provider: &str) -> bool {
        if !self.config.enabled {
            return false;
        }

        // If no providers specified, capture all
        if self.provider_filter.is_empty() {
            return true;
        }

        self.provider_filter.contains(&provider.to_lowercase())
    }

    /// Generate a new request ID.
    pub fn next_request_id(&self) -> u64 {
        REQUEST_COUNTER.fetch_add(1, Ordering::SeqCst)
    }

    /// Record a captured interaction.
    pub async fn record(&self, interaction: CapturedInteraction) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        // Check if we should capture based on success/failure
        if interaction.success && !self.config.capture_success {
            debug!(
                "Skipping capture for successful request to {}",
                interaction.provider
            );
            return Ok(());
        }

        // Generate filename
        let filename = format!(
            "{}_{}_{}_{}.json",
            interaction.provider,
            interaction.tier_name,
            chrono::Utc::now().format("%Y%m%d_%H%M%S"),
            interaction.request_id
        );
        let filepath = self.output_path.join(&filename);

        // Serialize and write
        let json = serde_json::to_string_pretty(&interaction)?;
        let mut file = File::create(&filepath)?;
        file.write_all(json.as_bytes())?;

        info!(
            "Captured {} interaction: {} ({}ms, status={})",
            interaction.provider,
            filepath.display(),
            interaction.latency_ms,
            interaction.response_status
        );

        // Manage file rotation
        self.rotate_files().await?;

        Ok(())
    }

    /// Rotate old capture files if we exceed max_files.
    async fn rotate_files(&self) -> Result<()> {
        let mut count = self.file_count.write().await;

        // Only check periodically (every 100 captures)
        *count += 1;
        if *count % 100 != 0 {
            return Ok(());
        }

        let entries: Vec<_> = fs::read_dir(&self.output_path)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "json"))
            .collect();

        if entries.len() <= self.config.max_files {
            return Ok(());
        }

        // Sort by modification time (oldest first)
        let mut files_with_time: Vec<_> = entries
            .into_iter()
            .filter_map(|e| {
                e.metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(|t| (e.path(), t))
            })
            .collect();

        files_with_time.sort_by(|a, b| a.1.cmp(&b.1));

        // Delete oldest files
        let to_delete = files_with_time.len() - self.config.max_files;
        for (path, _) in files_with_time.into_iter().take(to_delete) {
            if let Err(e) = fs::remove_file(&path) {
                warn!(
                    "Failed to remove old capture file {}: {}",
                    path.display(),
                    e
                );
            } else {
                debug!("Rotated capture file: {}", path.display());
            }
        }

        Ok(())
    }

    /// List recent captures for a provider.
    pub fn list_captures(
        &self,
        provider: Option<&str>,
        limit: usize,
    ) -> Result<Vec<CapturedInteraction>> {
        let mut captures = Vec::new();

        let mut entries: Vec<_> = fs::read_dir(&self.output_path)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                let path = e.path();
                let is_json = path.extension().map_or(false, |ext| ext == "json");
                let matches_provider = provider.map_or(true, |p| {
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .map_or(false, |name| name.starts_with(&format!("{}_", p)))
                });
                is_json && matches_provider
            })
            .collect();

        // Sort by modification time (newest first)
        entries.sort_by(|a, b| {
            b.metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                .cmp(
                    &a.metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                )
        });

        for entry in entries.into_iter().take(limit) {
            match fs::read_to_string(entry.path()) {
                Ok(content) => {
                    if let Ok(capture) = serde_json::from_str::<CapturedInteraction>(&content) {
                        captures.push(capture);
                    }
                }
                Err(e) => {
                    warn!(
                        "Failed to read capture file {}: {}",
                        entry.path().display(),
                        e
                    );
                }
            }
        }

        Ok(captures)
    }

    /// Get statistics about captured interactions.
    pub fn get_stats(&self) -> Result<CaptureStats> {
        let mut stats = CaptureStats::default();

        for entry in fs::read_dir(&self.output_path)?.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "json") {
                stats.total_captures += 1;

                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(capture) = serde_json::from_str::<CapturedInteraction>(&content) {
                        *stats
                            .by_provider
                            .entry(capture.provider.clone())
                            .or_insert(0) += 1;

                        if capture.success {
                            stats.success_count += 1;
                        } else {
                            stats.error_count += 1;
                        }

                        stats.total_latency_ms += capture.latency_ms;
                    }
                }
            }
        }

        if stats.total_captures > 0 {
            stats.avg_latency_ms = stats.total_latency_ms / stats.total_captures as u64;
        }

        Ok(stats)
    }
}

/// Statistics from captured interactions.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CaptureStats {
    pub total_captures: usize,
    pub success_count: usize,
    pub error_count: usize,
    pub total_latency_ms: u64,
    pub avg_latency_ms: u64,
    pub by_provider: std::collections::HashMap<String, usize>,
}

/// Expand ~ to home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(&path[2..]);
        }
    }
    PathBuf::from(path)
}

/// Builder for captured interactions.
#[derive(Debug, Default)]
pub struct CaptureBuilder {
    request_id: u64,
    provider: String,
    tier_name: String,
    model: String,
    url: String,
    method: String,
    request_headers: Option<serde_json::Value>,
    request_body: serde_json::Value,
    start_time: Option<std::time::Instant>,
    is_streaming: bool,
    include_headers: bool,
    max_body_size: usize,
}

impl CaptureBuilder {
    pub fn new(request_id: u64, provider: impl Into<String>, tier_name: impl Into<String>) -> Self {
        Self {
            request_id,
            provider: provider.into(),
            tier_name: tier_name.into(),
            method: "POST".to_string(),
            max_body_size: default_max_body_size(),
            ..Default::default()
        }
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    pub fn method(mut self, method: impl Into<String>) -> Self {
        self.method = method.into();
        self
    }

    pub fn request_body(mut self, body: serde_json::Value) -> Self {
        self.request_body = body;
        self
    }

    pub fn request_headers(mut self, headers: serde_json::Value) -> Self {
        self.request_headers = Some(headers);
        self
    }

    pub fn streaming(mut self, is_streaming: bool) -> Self {
        self.is_streaming = is_streaming;
        self
    }

    pub fn max_body_size(mut self, size: usize) -> Self {
        self.max_body_size = size;
        self
    }

    pub fn include_headers(mut self, include: bool) -> Self {
        self.include_headers = include;
        self
    }

    pub fn start(mut self) -> Self {
        self.start_time = Some(std::time::Instant::now());
        self
    }

    /// Complete the capture with response data.
    pub fn complete(
        self,
        status: u16,
        response_body: &str,
        response_headers: Option<serde_json::Value>,
        error: Option<String>,
    ) -> CapturedInteraction {
        let latency_ms = self
            .start_time
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0);

        let (body, truncated) =
            if self.max_body_size > 0 && response_body.len() > self.max_body_size {
                (response_body[..self.max_body_size].to_string(), true)
            } else {
                (response_body.to_string(), false)
            };

        CapturedInteraction {
            request_id: self.request_id,
            provider: self.provider,
            tier_name: self.tier_name,
            model: self.model,
            timestamp: chrono::Utc::now().to_rfc3339(),
            url: self.url,
            method: self.method,
            request_headers: if self.include_headers {
                self.request_headers
            } else {
                None
            },
            request_body: self.request_body,
            response_status: status,
            response_headers: if self.include_headers {
                response_headers
            } else {
                None
            },
            response_body: body,
            response_truncated: truncated,
            latency_ms,
            is_streaming: self.is_streaming,
            success: status >= 200 && status < 300,
            error,
            metadata: None,
        }
    }

    /// Complete with an error (no response received).
    pub fn complete_with_error(self, error: impl Into<String>) -> CapturedInteraction {
        self.complete(0, "", None, Some(error.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_capture_builder() {
        let capture = CaptureBuilder::new(1, "minimax", "ccr-mm")
            .model("MiniMax-M2.1")
            .url("https://api.minimax.chat/v1/chat/completions")
            .request_body(serde_json::json!({"model": "test", "messages": []}))
            .streaming(false)
            .start()
            .complete(200, r#"{"choices": []}"#, None, None);

        assert_eq!(capture.provider, "minimax");
        assert_eq!(capture.tier_name, "ccr-mm");
        assert_eq!(capture.response_status, 200);
        assert!(capture.success);
        assert!(capture.error.is_none());
    }

    #[test]
    fn test_capture_builder_error() {
        let capture = CaptureBuilder::new(2, "minimax", "ccr-mm")
            .model("MiniMax-M2.1")
            .url("https://api.minimax.chat/v1/chat/completions")
            .request_body(serde_json::json!({"model": "test"}))
            .complete_with_error("Connection timeout");

        assert!(!capture.success);
        assert_eq!(capture.error, Some("Connection timeout".to_string()));
        assert_eq!(capture.response_status, 0);
    }

    #[test]
    fn test_truncation() {
        let long_body = "x".repeat(2000);
        let capture = CaptureBuilder::new(3, "minimax", "ccr-mm")
            .request_body(serde_json::json!({}))
            .max_body_size(1000)
            .complete(200, &long_body, None, None);

        assert!(capture.response_truncated);
        assert_eq!(capture.response_body.len(), 1000);
    }

    #[tokio::test]
    async fn test_debug_capture_manager() {
        let dir = tempdir().unwrap();
        let config = DebugCaptureConfig {
            enabled: true,
            providers: vec!["minimax".to_string()],
            output_dir: dir.path().to_string_lossy().to_string(),
            max_files: 10,
            ..Default::default()
        };

        let capture_mgr = DebugCapture::new(config).unwrap();

        assert!(capture_mgr.should_capture("minimax"));
        assert!(capture_mgr.should_capture("Minimax")); // case insensitive
        assert!(!capture_mgr.should_capture("deepseek"));

        // Record a capture
        let interaction = CaptureBuilder::new(1, "minimax", "ccr-mm")
            .model("MiniMax-M2.1")
            .request_body(serde_json::json!({"test": true}))
            .complete(200, r#"{"result": "ok"}"#, None, None);

        capture_mgr.record(interaction).await.unwrap();

        // Verify file was created
        let captures = capture_mgr.list_captures(Some("minimax"), 10).unwrap();
        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0].provider, "minimax");
    }

    #[test]
    fn test_expand_tilde() {
        let path = expand_tilde("~/.ccr-rust/captures");
        assert!(!path.as_os_str().to_string_lossy().contains('~'));
    }

    #[test]
    fn test_should_capture_all_when_empty() {
        let config = DebugCaptureConfig {
            enabled: true,
            providers: vec![], // Empty means capture all
            ..Default::default()
        };

        let capture_mgr = DebugCapture::new(config).unwrap();
        assert!(capture_mgr.should_capture("minimax"));
        assert!(capture_mgr.should_capture("deepseek"));
        assert!(capture_mgr.should_capture("openrouter"));
    }

    #[test]
    fn test_disabled_capture() {
        let config = DebugCaptureConfig {
            enabled: false,
            providers: vec!["minimax".to_string()],
            ..Default::default()
        };

        let capture_mgr = DebugCapture::new(config).unwrap();
        assert!(!capture_mgr.should_capture("minimax"));
    }
}
