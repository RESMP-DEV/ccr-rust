// SPDX-License-Identifier: AGPL-3.0-or-later
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
//!     "max_files": 100,
//!     "include_headers": false
//!   }
//! }
//! ```
//!
//! Captured files are stored as JSON with timestamped filenames:
//! `ccr_capture_v1_{provider}_{tier}_{timestamp}_{request_id}.json`

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Global request counter for unique IDs.
static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

const CAPTURE_FILE_PREFIX: &str = "ccr_capture_v1_";
const DEFAULT_MAX_FILES: usize = 100;
const HARD_MAX_FILES: usize = 1000;
const MAX_CAPTURE_FILE_BYTES: usize = 4 * 1024 * 1024;

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

    /// Maximum number of CCR-owned capture files to keep (oldest are deleted).
    /// Values outside the supported bounded range are replaced or clamped.
    #[serde(default = "default_max_files")]
    pub max_files: usize,

    /// Include raw HTTP headers in capture.
    #[serde(default)]
    pub include_headers: bool,

    /// Capture response body even on success (normally only captures on error).
    #[serde(default = "default_capture_success")]
    pub capture_success: bool,

    /// Maximum response body size to capture (bytes).
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
    DEFAULT_MAX_FILES
}

fn default_capture_success() -> bool {
    false
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
    retention_lock: Mutex<()>,
}

impl DebugCapture {
    /// Create a new debug capture manager.
    pub fn new(mut config: DebugCaptureConfig) -> Result<Self> {
        if config.max_files == 0 {
            warn!(
                default = DEFAULT_MAX_FILES,
                "Debug capture max_files must be bounded; using the default"
            );
            config.max_files = DEFAULT_MAX_FILES;
        } else if config.max_files > HARD_MAX_FILES {
            warn!(
                requested = config.max_files,
                maximum = HARD_MAX_FILES,
                "Debug capture max_files exceeds the hard limit; clamping"
            );
            config.max_files = HARD_MAX_FILES;
        }

        let output_path = expand_tilde(&config.output_dir);

        // Capture is opt-in. Merely parsing or constructing the default
        // configuration must not create a directory on disk.
        if config.enabled {
            ensure_private_directory(&output_path)?;
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
            retention_lock: Mutex::new(()),
        })
    }

    /// Check if capture is enabled for a given provider.
    pub fn should_capture(&self, provider: &str) -> bool {
        if !self.config.enabled {
            debug!("should_capture: disabled globally");
            return false;
        }

        // If no providers specified, capture all
        if self.provider_filter.is_empty() {
            debug!("should_capture: capturing all (empty filter)");
            return true;
        }

        let result = self.provider_filter.contains(&provider.to_lowercase());
        debug!(
            "should_capture: provider={}, filter={:?}, result={}",
            provider, self.provider_filter, result
        );

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
            "{}{}_{}_{}_{}.json",
            CAPTURE_FILE_PREFIX,
            sanitize_filename_segment(&interaction.provider),
            sanitize_filename_segment(&interaction.tier_name),
            chrono::Utc::now().format("%Y%m%d_%H%M%S_%f"),
            interaction.request_id
        );
        let filepath = self.output_path.join(&filename);

        // Serialize and create a new private file without following or
        // replacing an existing path. The hard byte cap complements bounded
        // file retention so capture cannot grow without limit.
        let json = serde_json::to_string_pretty(&interaction)?;
        if json.len() > MAX_CAPTURE_FILE_BYTES {
            bail!(
                "debug capture exceeds the {} byte file limit",
                MAX_CAPTURE_FILE_BYTES
            );
        }
        let write_result = (|| -> Result<()> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&filepath)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            file.write_all(json.as_bytes())?;
            file.sync_all()?;
            Ok(())
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_file(&filepath);
            return Err(error);
        }

        // Manage file rotation
        if let Err(error) = self.rotate_files().await {
            // Never make a failed retention pass increase disk usage.
            let _ = fs::remove_file(&filepath);
            return Err(error);
        }

        info!(
            "Captured {} interaction: {} ({}ms, status={})",
            interaction.provider,
            filepath.display(),
            interaction.latency_ms,
            interaction.response_status
        );

        Ok(())
    }

    /// Rotate old CCR-owned capture files if we exceed max_files.
    ///
    /// Legacy JSON files, task captures, symlinks, and other unrelated files
    /// are deliberately ignored so enabling the new policy cannot erase an
    /// existing corpus.
    async fn rotate_files(&self) -> Result<()> {
        let _guard = self.retention_lock.lock().await;
        let entries: Vec<_> = fs::read_dir(&self.output_path)?
            .filter_map(|e| e.ok())
            .filter(is_managed_capture_file)
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

        files_with_time
            .sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));

        // Delete oldest files
        let to_delete = files_with_time.len() - self.config.max_files;
        let mut deleted = 0;
        let mut last_error = None;
        for (path, _) in files_with_time {
            if deleted >= to_delete {
                break;
            }
            match fs::remove_file(&path) {
                Ok(()) => {
                    deleted += 1;
                    debug!("Rotated capture file: {}", path.display());
                }
                Err(error) => {
                    warn!(
                        "Failed to remove old capture file {}: {}",
                        path.display(),
                        error
                    );
                    last_error = Some(error);
                }
            }
        }
        if deleted < to_delete {
            let detail = last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "no removable CCR-owned files".to_string());
            bail!("debug capture retention could not enforce its limit: {detail}")
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
            .filter(is_managed_capture_file)
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

        let bounded_limit = limit.min(HARD_MAX_FILES);
        for entry in entries {
            if captures.len() >= bounded_limit {
                break;
            }
            match read_managed_capture(&entry.path()) {
                Ok(capture) => {
                    if provider.is_none_or(|expected| capture.provider == expected) {
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

        for entry in fs::read_dir(&self.output_path)?
            .filter_map(|entry| entry.ok())
            .filter(is_managed_capture_file)
            .take(HARD_MAX_FILES)
        {
            if let Ok(capture) = read_managed_capture(&entry.path()) {
                stats.total_captures += 1;
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
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

fn ensure_private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("debug capture directory must not be a symlink")
        }
        Ok(metadata) if !metadata.is_dir() => {
            bail!("debug capture path exists but is not a directory")
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
        }
        Err(error) => return Err(error.into()),
    }

    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("debug capture path is not a private directory")
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }

    Ok(())
}

fn sanitize_filename_segment(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .take(64)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "unknown".to_string()
    } else {
        sanitized
    }
}

fn is_managed_capture_file(entry: &fs::DirEntry) -> bool {
    let Ok(file_type) = entry.file_type() else {
        return false;
    };
    if !file_type.is_file() {
        return false;
    }
    entry
        .file_name()
        .to_str()
        .is_some_and(|name| name.starts_with(CAPTURE_FILE_PREFIX) && name.ends_with(".json"))
}

fn read_managed_capture(path: &Path) -> Result<CapturedInteraction> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        bail!("debug capture entry is not a regular file")
    }

    let mut file = fs::File::open(path)?;
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len())
            .unwrap_or(MAX_CAPTURE_FILE_BYTES)
            .min(MAX_CAPTURE_FILE_BYTES),
    );
    Read::take(&mut file, (MAX_CAPTURE_FILE_BYTES + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CAPTURE_FILE_BYTES {
        bail!("debug capture exceeds the read limit")
    }
    Ok(serde_json::from_slice(&bytes)?)
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
            success: (200..300).contains(&status),
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

    fn interaction(request_id: u64) -> CapturedInteraction {
        CaptureBuilder::new(request_id, "minimax", "ccr-mm")
            .model("MiniMax-M2.5")
            .request_body(serde_json::json!({"request": request_id}))
            .complete(500, r#"{"error": "test"}"#, None, Some("test".to_string()))
    }

    #[test]
    fn test_capture_builder() {
        let capture = CaptureBuilder::new(1, "minimax", "ccr-mm")
            .model("MiniMax-M2.5")
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
            .model("MiniMax-M2.5")
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
            .model("MiniMax-M2.5")
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

    #[test]
    fn test_capture_is_disabled_when_configuration_is_absent() {
        let config: DebugCaptureConfig = serde_json::from_str("{}").unwrap();
        assert!(!config.enabled);
        assert!(!config.capture_success);

        let dir = tempdir().unwrap();
        let output = dir.path().join("not-created");
        let manager = DebugCapture::new(DebugCaptureConfig {
            output_dir: output.to_string_lossy().to_string(),
            ..config
        })
        .unwrap();

        assert!(!manager.should_capture("minimax"));
        assert!(!output.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_capture_directory_and_file_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let output = dir.path().join("captures");
        fs::create_dir(&output).unwrap();
        fs::set_permissions(&output, fs::Permissions::from_mode(0o755)).unwrap();

        let manager = DebugCapture::new(DebugCaptureConfig {
            enabled: true,
            output_dir: output.to_string_lossy().to_string(),
            ..Default::default()
        })
        .unwrap();
        manager.record(interaction(11)).await.unwrap();

        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let capture_path = fs::read_dir(&output)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .find(is_managed_capture_file)
            .unwrap()
            .path();
        assert_eq!(
            fs::metadata(capture_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[tokio::test]
    async fn test_retention_is_immediate_bounded_and_preserves_legacy_files() {
        let dir = tempdir().unwrap();
        let output = dir.path().join("captures");
        fs::create_dir(&output).unwrap();
        let legacy = output.join("legacy_capture.json");
        fs::write(&legacy, "legacy").unwrap();

        #[cfg(unix)]
        let legacy_symlink = {
            use std::os::unix::fs::symlink;
            let path = output.join("ccr_capture_v1_legacy_symlink.json");
            symlink(&legacy, &path).unwrap();
            Some(path)
        };

        let manager = DebugCapture::new(DebugCaptureConfig {
            enabled: true,
            output_dir: output.to_string_lossy().to_string(),
            max_files: 2,
            ..Default::default()
        })
        .unwrap();

        for request_id in 1..=3 {
            manager.record(interaction(request_id)).await.unwrap();
        }

        let managed_count = fs::read_dir(&output)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(is_managed_capture_file)
            .count();
        assert_eq!(managed_count, 2);
        assert_eq!(manager.list_captures(None, usize::MAX).unwrap().len(), 2);
        assert_eq!(manager.get_stats().unwrap().total_captures, 2);
        assert_eq!(fs::read_to_string(&legacy).unwrap(), "legacy");
        #[cfg(unix)]
        assert!(legacy_symlink.unwrap().symlink_metadata().is_ok());
    }

    #[test]
    fn test_unbounded_retention_value_is_replaced() {
        let manager = DebugCapture::new(DebugCaptureConfig {
            max_files: 0,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(manager.config.max_files, DEFAULT_MAX_FILES);

        let manager = DebugCapture::new(DebugCaptureConfig {
            max_files: HARD_MAX_FILES + 1,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(manager.config.max_files, HARD_MAX_FILES);
    }

    #[cfg(unix)]
    #[test]
    fn test_capture_directory_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let actual = dir.path().join("actual");
        let linked = dir.path().join("linked");
        fs::create_dir(&actual).unwrap();
        symlink(&actual, &linked).unwrap();

        let result = DebugCapture::new(DebugCaptureConfig {
            enabled: true,
            output_dir: linked.to_string_lossy().to_string(),
            ..Default::default()
        });

        assert!(result.is_err());
    }
}
