// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use crate::config::Config;
use crate::debug_capture::DebugCapture;
#[cfg(feature = "gp")]
use crate::gp_router::GpRequestRouter;
use crate::ratelimit::RateLimitTracker;
use crate::routing::EwmaTracker;
use crate::transformer::TransformerRegistry;

// ============================================================================
// Error Types
// ============================================================================

/// Error type for try_request that distinguishes rate limits from other errors.
#[derive(Debug)]
pub enum TryRequestError {
    /// 429 Too Many Requests - includes optional Retry-After header value
    RateLimited(Option<std::time::Duration>),
    /// Other errors
    Other(anyhow::Error),
}

impl std::fmt::Display for TryRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TryRequestError::RateLimited(d) => {
                write!(f, "Rate limited")?;
                if let Some(dur) = d {
                    write!(f, " (retry after {}s)", dur.as_secs())?;
                }
                Ok(())
            }
            TryRequestError::Other(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for TryRequestError {}

impl From<anyhow::Error> for TryRequestError {
    fn from(e: anyhow::Error) -> Self {
        TryRequestError::Other(e)
    }
}

// ============================================================================
// Application State
// ============================================================================

/// Shared application state threaded through Axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub ewma_tracker: Arc<EwmaTracker>,
    #[cfg(feature = "gp")]
    pub gp_router: Option<Arc<GpRequestRouter>>,
    #[cfg(not(feature = "gp"))]
    pub gp_router: Option<()>,
    pub transformer_registry: Arc<TransformerRegistry>,
    pub active_streams: Arc<AtomicUsize>,
    pub max_streams: usize,
    pub ratelimit_tracker: Arc<RateLimitTracker>,
    #[allow(dead_code)]
    pub shutdown_timeout: u64,
    /// Debug capture manager for recording raw API interactions.
    pub debug_capture: Option<Arc<DebugCapture>>,
}

// ============================================================================
// Anthropic Format Types (Input)
// ============================================================================

#[derive(Debug, Serialize, Deserialize)]
pub struct AnthropicRequest {
    pub model: String,
    pub messages: Vec<Message>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<serde_json::Value>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,

    /// When the original inbound request was already OpenAI-formatted (e.g. from
    /// a Codex frontend), we stash the raw JSON here so that
    /// `try_request_via_openai_protocol` can send it directly to an
    /// OpenAI-compatible backend without the wasteful
    /// `OpenAI → Anthropic → OpenAI` round-trip translation.
    #[serde(skip)]
    pub openai_passthrough_body: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Message {
    pub role: String,
    pub content: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

// ============================================================================
// OpenAI Format Types
// ============================================================================

/// OpenAI chat completion request format.
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenAIRequest {
    pub model: String,
    pub messages: Vec<OpenAIMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

/// OpenAI message format.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct OpenAIMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAIToolCall>>,
}

/// OpenAI non-streaming response.
#[derive(Debug, Deserialize)]
pub struct OpenAIResponse {
    pub id: String,
    #[allow(dead_code)]
    pub object: String,
    #[allow(dead_code)]
    pub created: i64,
    #[allow(dead_code)]
    pub model: String,
    pub choices: Vec<OpenAIChoice>,
    pub usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAIChoice {
    #[allow(dead_code)]
    pub index: u32,
    pub message: OpenAIResponseMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OpenAIResponseMessage {
    #[allow(dead_code)]
    pub role: String,
    #[serde(default)]
    pub content: Option<serde_json::Value>,
    #[serde(default, rename = "reasoning_content", alias = "reasoning")]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OpenAIToolCall {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default, rename = "type")]
    pub tool_type: Option<String>,
    #[serde(default)]
    pub function: Option<OpenAIToolFunction>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OpenAIToolFunction {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub arguments: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct OpenAIUsage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    #[allow(dead_code)]
    pub prompt_tokens_details: Option<serde_json::Value>,
}

/// OpenAI streaming response chunk.
#[derive(Debug, Deserialize)]
pub struct OpenAIStreamChunk {
    pub id: String,
    #[allow(dead_code)]
    pub object: String,
    #[allow(dead_code)]
    pub created: i64,
    pub model: String,
    pub choices: Vec<OpenAIStreamChoice>,
    pub usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAIStreamChoice {
    #[allow(dead_code)]
    pub index: u32,
    pub delta: OpenAIDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct OpenAIDelta {
    #[serde(default)]
    #[allow(dead_code)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default, rename = "reasoning_content", alias = "reasoning")]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<OpenAIStreamToolCall>>,
}

#[derive(Debug, Deserialize, Default)]
pub struct OpenAIStreamToolCall {
    #[serde(default)]
    pub index: usize,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<OpenAIStreamToolFunction>,
}

#[derive(Debug, Deserialize, Default)]
pub struct OpenAIStreamToolFunction {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

// ============================================================================
// Anthropic Response Types (Output)
// ============================================================================

/// Anthropic non-streaming response format.
#[derive(Debug, Serialize, Deserialize)]
pub struct AnthropicResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub response_type: String,
    pub role: String,
    pub model: String,
    pub content: Vec<AnthropicContentBlock>,
    pub usage: AnthropicUsage,
    pub stop_reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String, signature: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct AnthropicUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Anthropic streaming event types.
#[derive(Debug, Serialize)]
pub struct AnthropicStreamEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_block: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<AnthropicUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
}
