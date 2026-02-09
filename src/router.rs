use axum::{
    body::{to_bytes, Body},
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use tracing::{error, info, trace, warn};

use crate::config::{Config, ProviderProtocol};
use crate::frontend::codex::CodexFrontend;
use crate::frontend::{detect_frontend, Frontend};
use crate::metrics::{
    record_failure, record_pre_request_tokens, record_rate_limit_backoff, record_rate_limit_hit,
    record_request_duration_with_frontend, record_request_with_frontend, record_usage,
    sync_ewma_gauge, verify_token_usage,
};
use crate::ratelimit::RateLimitTracker;
use crate::routing::{AttemptTimer, EwmaTracker};
use crate::sse::{SseFrameDecoder, StreamVerifyCtx};
use crate::transform::anthropic_to_openai::AnthropicToOpenAiResponseTransformer;
use crate::transform::openai_to_anthropic::OpenAiToAnthropicTransformer;
use crate::transformer::{Transformer, TransformerChain, TransformerRegistry};

/// Extract rate limit information from upstream response headers.
fn extract_rate_limit_headers(
    resp: &reqwest::Response,
) -> (Option<u32>, Option<std::time::Instant>) {
    let remaining = resp
        .headers()
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok());

    let reset_at = resp
        .headers()
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(|ts| {
            std::time::Instant::now()
                + std::time::Duration::from_secs(
                    ts.saturating_sub(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                    ),
                )
        });

    (remaining, reset_at)
}

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

/// Shared application state threaded through Axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub ewma_tracker: Arc<EwmaTracker>,
    pub transformer_registry: Arc<TransformerRegistry>,
    pub active_streams: Arc<AtomicUsize>,
    pub ratelimit_tracker: Arc<RateLimitTracker>,
    #[allow(dead_code)]
    pub shutdown_timeout: u64,
}

// ============================================================================
// Anthropic Format Types (Input)
// ============================================================================

#[derive(Debug, Serialize, Deserialize)]
pub struct AnthropicRequest {
    pub model: String,
    pub messages: Vec<Message>,

    #[serde(default)]
    pub system: Option<serde_json::Value>,

    #[serde(default)]
    pub max_tokens: Option<u32>,

    #[serde(default)]
    pub temperature: Option<f32>,

    #[serde(default)]
    pub stream: Option<bool>,

    #[serde(default)]
    pub tools: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Message {
    pub role: String,
    pub content: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub tier: String,
    pub attempts: usize,
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
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
}

/// OpenAI message format.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct OpenAIMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAIToolCall>>,
}

/// OpenAI non-streaming response.
#[derive(Debug, Deserialize)]
struct OpenAIResponse {
    id: String,
    #[allow(dead_code)] // TODO: Log for debugging
    object: String,
    #[allow(dead_code)] // TODO: Include in response headers
    created: i64,
    #[allow(dead_code)] // TODO: Validate model matches requested
    model: String,
    choices: Vec<OpenAIChoice>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    #[allow(dead_code)] // TODO: Use for multi-choice selection
    index: u32,
    message: OpenAIResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct OpenAIResponseMessage {
    #[allow(dead_code)] // TODO: Validate response role
    role: String,
    #[serde(default)]
    content: Option<serde_json::Value>,
    #[serde(default, rename = "reasoning_content")]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OpenAIToolCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(default, rename = "type")]
    tool_type: Option<String>,
    #[serde(default)]
    function: Option<OpenAIToolFunction>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OpenAIToolFunction {
    #[serde(default)]
    name: String,
    #[serde(default)]
    arguments: String,
}

#[derive(Debug, Deserialize, Default)]
struct OpenAIUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    #[allow(dead_code)] // TODO: Track token breakdown for analytics
    prompt_tokens_details: Option<serde_json::Value>,
}

/// OpenAI streaming response chunk.
#[derive(Debug, Deserialize)]
struct OpenAIStreamChunk {
    id: String,
    #[allow(dead_code)] // TODO: Log stream type for debugging
    object: String,
    #[allow(dead_code)] // TODO: Track stream latency
    created: i64,
    model: String,
    choices: Vec<OpenAIStreamChoice>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamChoice {
    #[allow(dead_code)] // TODO: Support multi-stream selection
    index: u32,
    delta: OpenAIDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct OpenAIDelta {
    #[serde(default)]
    #[allow(dead_code)] // TODO: First chunk role validation
    role: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default, rename = "reasoning_content")]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAIStreamToolCall>>,
}

#[derive(Debug, Deserialize, Default)]
struct OpenAIStreamToolCall {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<OpenAIStreamToolFunction>,
}

#[derive(Debug, Deserialize, Default)]
struct OpenAIStreamToolFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
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
struct AnthropicStreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_block: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delta: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<AnthropicUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_reason: Option<String>,
}

// ============================================================================
// Request Translation: Anthropic -> OpenAI
// ============================================================================

/// Convert Anthropic message content to OpenAI content format.
///
/// Preserves multimodal blocks where possible:
/// - text blocks => OpenAI text blocks
/// - image blocks => OpenAI image_url blocks
/// - thinking blocks => rendered inline as text for compatibility
fn normalize_message_content(content: &serde_json::Value) -> serde_json::Value {
    match content {
        serde_json::Value::String(s) => serde_json::Value::String(s.clone()),
        serde_json::Value::Array(arr) => {
            let mut openai_blocks = Vec::new();
            let mut text_fallback = String::new();
            for block in arr {
                let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match block_type {
                    "text" => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            openai_blocks.push(serde_json::json!({
                                "type": "text",
                                "text": text
                            }));
                        }
                    }
                    "image" => {
                        if let Some(source) = block.get("source") {
                            if let Some(data) = source.get("data").and_then(|v| v.as_str()) {
                                let media_type = source
                                    .get("media_type")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("image/jpeg");
                                openai_blocks.push(serde_json::json!({
                                    "type": "image_url",
                                    "image_url": {
                                        "url": format!("data:{};base64,{}", media_type, data)
                                    }
                                }));
                            } else if let Some(url) = source.get("url").and_then(|v| v.as_str()) {
                                openai_blocks.push(serde_json::json!({
                                    "type": "image_url",
                                    "image_url": {
                                        "url": url
                                    }
                                }));
                            }
                        }
                    }
                    "thinking" => {
                        if let Some(thinking) = block.get("thinking").and_then(|t| t.as_str()) {
                            if !text_fallback.is_empty() {
                                text_fallback.push_str("\n\n");
                            }
                            text_fallback.push_str("<thinking>");
                            text_fallback.push_str(thinking);
                            text_fallback.push_str("</thinking>");
                        }
                    }
                    _ => {
                        if !text_fallback.is_empty() {
                            text_fallback.push('\n');
                        }
                        text_fallback.push_str(&block.to_string());
                    }
                }
            }

            let text_only_blocks = !openai_blocks.is_empty()
                && text_fallback.is_empty()
                && openai_blocks
                    .iter()
                    .all(|block| block.get("type").and_then(|t| t.as_str()) == Some("text"));

            if text_only_blocks {
                let concatenated_text = openai_blocks
                    .iter()
                    .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                    .collect::<String>();
                serde_json::Value::String(concatenated_text)
            } else if openai_blocks.is_empty() {
                serde_json::Value::String(text_fallback)
            } else {
                if !text_fallback.is_empty() {
                    openai_blocks.push(serde_json::json!({
                        "type": "text",
                        "text": text_fallback
                    }));
                }
                serde_json::Value::Array(openai_blocks)
            }
        }
        _ => content.clone(),
    }
}

fn has_nonempty_content(content: &serde_json::Value) -> bool {
    match content {
        serde_json::Value::Null => false,
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Object(o) => !o.is_empty(),
        _ => true,
    }
}

/// Convert Anthropic tool format to OpenAI tool format.
///
/// Anthropic: `{name, description, input_schema}`
/// OpenAI: `{type: "function", function: {name, description, parameters}}`
fn convert_anthropic_tools_to_openai(
    tools: &Option<Vec<serde_json::Value>>,
) -> Option<Vec<serde_json::Value>> {
    tools.as_ref().map(|tool_list| {
        tool_list
            .iter()
            .map(|tool| {
                // Check if already in OpenAI format (has "type": "function")
                if tool.get("type").and_then(|t| t.as_str()) == Some("function") {
                    return tool.clone();
                }

                // Convert from Anthropic format to OpenAI format
                let name = tool.get("name").cloned().unwrap_or(serde_json::Value::Null);
                let description = tool.get("description").cloned();
                let parameters = tool
                    .get("input_schema")
                    .cloned()
                    .unwrap_or(serde_json::json!({"type": "object", "properties": {}}));

                let mut function = serde_json::json!({
                    "name": name,
                    "parameters": parameters,
                });

                if let Some(desc) = description {
                    function["description"] = desc;
                }

                serde_json::json!({
                    "type": "function",
                    "function": function,
                })
            })
            .collect()
    })
}

/// Translate Anthropic request format to OpenAI format.
fn translate_request_anthropic_to_openai(
    anthropic_req: &AnthropicRequest,
    model: &str,
) -> OpenAIRequest {
    let mut messages: Vec<OpenAIMessage> = Vec::new();
    let is_reasoning_model = model.to_lowercase().contains("reasoner")
        || model.to_lowercase().contains("r1")
        || model.to_lowercase().contains("thinking");

    // Handle system prompt: Anthropic has it as a top-level field,
    // OpenAI expects it as the first message with role "system"
    if let Some(system) = &anthropic_req.system {
        let system_content = match system {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(arr) => {
                let mut result = String::new();
                for block in arr {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        if !result.is_empty() {
                            result.push('\n');
                        }
                        result.push_str(text);
                    }
                }
                result
            }
            _ => system.to_string(),
        };

        if !system_content.is_empty() {
            messages.push(OpenAIMessage {
                role: "system".to_string(),
                content: Some(serde_json::Value::String(system_content)),
                reasoning_content: None,
                tool_call_id: None,
                tool_calls: None,
            });
        }
    }

    // Convert user and assistant messages
    for msg in &anthropic_req.messages {
        let role = match msg.role.as_str() {
            "human" | "user" => "user",
            "assistant" => "assistant",
            r => r,
        };

        // Handle tool_result blocks in user messages
        if let Some(arr) = msg.content.as_array() {
            for block in arr {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                    // Extract content and tool_use_id from tool_result block
                    let tool_content = block.get("content").and_then(|c| {
                        // Content can be a string or an object with type "text"
                        if let Some(s) = c.as_str() {
                            Some(s.to_string())
                        } else if let Some(obj) = c.as_object() {
                            obj.get("text").and_then(|t| t.as_str()).map(String::from)
                        } else if let Some(arr) = c.as_array() {
                            // Handle array of content items in tool_result
                            let mut result = String::new();
                            for item in arr {
                                if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                    result.push_str(text);
                                } else if let Some(text) = item.as_str() {
                                    result.push_str(text);
                                }
                            }
                            Some(result)
                        } else {
                            None
                        }
                    });

                    let tool_call_id = block
                        .get("tool_use_id")
                        .and_then(|id| id.as_str())
                        .map(String::from);

                    messages.push(OpenAIMessage {
                        role: "tool".to_string(),
                        content: tool_content.map(serde_json::Value::String),
                        tool_call_id,
                        ..Default::default()
                    });
                }
            }
        }

        let mut content_source = msg.content.clone();
        let mut tool_calls: Option<Vec<OpenAIToolCall>> = None;
        let mut reasoning_content: Option<String> = None;

        // Convert Anthropic assistant tool_use blocks into OpenAI tool_calls.
        // Keep non-tool content blocks as assistant content.
        if role == "assistant" {
            if let Some(arr) = msg.content.as_array() {
                let mut filtered_blocks: Vec<serde_json::Value> = Vec::new();
                let mut converted_tool_calls: Vec<OpenAIToolCall> = Vec::new();
                let mut reasoning_parts: Vec<String> = Vec::new();

                for block in arr {
                    match block.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                        "tool_use" => {
                            let id = block
                                .get("id")
                                .and_then(|v| v.as_str())
                                .filter(|v| !v.is_empty())
                                .unwrap_or("toolu_unknown")
                                .to_string();
                            let name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .filter(|v| !v.is_empty())
                                .unwrap_or("tool")
                                .to_string();
                            let arguments = block
                                .get("input")
                                .cloned()
                                .unwrap_or_else(|| serde_json::json!({}))
                                .to_string();

                            converted_tool_calls.push(OpenAIToolCall {
                                id: Some(id),
                                tool_type: Some("function".to_string()),
                                function: Some(OpenAIToolFunction { name, arguments }),
                            });
                        }
                        "thinking" => {
                            if let Some(thinking) = block.get("thinking").and_then(|t| t.as_str()) {
                                if !thinking.is_empty() {
                                    reasoning_parts.push(thinking.to_string());
                                }
                            }
                        }
                        _ => filtered_blocks.push(block.clone()),
                    }
                }

                if !converted_tool_calls.is_empty() {
                    tool_calls = Some(converted_tool_calls);
                    content_source = serde_json::Value::Array(filtered_blocks);
                }
                if !reasoning_parts.is_empty() {
                    reasoning_content = Some(reasoning_parts.join("\n\n"));
                }
            }

            // DeepSeek reasoning models require `reasoning_content` to be present
            // on assistant turns that participate in tool calling.
            if is_reasoning_model && reasoning_content.is_none() {
                reasoning_content = Some(String::new());
            }
        }

        // Handle regular message content
        let content = normalize_message_content(&content_source);

        if has_nonempty_content(&content) || role != "user" {
            messages.push(OpenAIMessage {
                role: role.to_string(),
                content: Some(content),
                reasoning_content,
                tool_call_id: msg.tool_call_id.clone(),
                tool_calls,
            });
        }
    }

    OpenAIRequest {
        model: model.to_string(),
        messages,
        // Use max_completion_tokens for reasoning models to allow for reasoning
        max_tokens: if is_reasoning_model {
            None
        } else {
            anthropic_req.max_tokens
        },
        max_completion_tokens: if is_reasoning_model {
            anthropic_req.max_tokens
        } else {
            None
        },
        temperature: anthropic_req.temperature,
        stream: anthropic_req.stream,
        tools: convert_anthropic_tools_to_openai(&anthropic_req.tools),
        reasoning_effort: if is_reasoning_model {
            Some("high".to_string())
        } else {
            None
        },
    }
}

// ============================================================================
// Response Translation: OpenAI -> Anthropic
// ============================================================================

/// Translate OpenAI non-streaming response to Anthropic format.
fn translate_response_openai_to_anthropic(
    openai_resp: OpenAIResponse,
    model: &str,
) -> AnthropicResponse {
    let response_model = if openai_resp.model.is_empty() {
        model.to_string()
    } else {
        openai_resp.model.clone()
    };

    let content = if let Some(choice) = openai_resp.choices.first() {
        let mut blocks: Vec<AnthropicContentBlock> = Vec::new();

        // Include reasoning content if present (from reasoning models)
        if let Some(reasoning) = &choice.message.reasoning_content {
            if !reasoning.is_empty() {
                blocks.push(AnthropicContentBlock::Thinking {
                    thinking: reasoning.clone(),
                    signature: String::new(), // OpenAI doesn't provide signatures
                });
            }
        }

        // Main content
        if let Some(content_value) = &choice.message.content {
            match content_value {
                serde_json::Value::String(text) if !text.is_empty() => {
                    blocks.push(AnthropicContentBlock::Text { text: text.clone() });
                }
                serde_json::Value::Array(items) => {
                    for item in items {
                        let block_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        match block_type {
                            "text" => {
                                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                                    if !text.is_empty() {
                                        blocks.push(AnthropicContentBlock::Text {
                                            text: text.to_string(),
                                        });
                                    }
                                }
                            }
                            "image_url" => {
                                blocks.push(AnthropicContentBlock::Text {
                                    text: item.to_string(),
                                });
                            }
                            _ => {}
                        }
                    }
                }
                other => {
                    if !other.is_null() {
                        blocks.push(AnthropicContentBlock::Text {
                            text: other.to_string(),
                        });
                    }
                }
            }
        }

        if let Some(tool_calls) = &choice.message.tool_calls {
            for (index, tool_call) in tool_calls.iter().enumerate() {
                if tool_call.tool_type.as_deref().unwrap_or("function") != "function" {
                    continue;
                }
                if let Some(function) = &tool_call.function {
                    let input = serde_json::from_str::<serde_json::Value>(&function.arguments)
                        .unwrap_or_else(|_| {
                            serde_json::json!({
                                "raw_arguments": function.arguments
                            })
                        });

                    blocks.push(AnthropicContentBlock::ToolUse {
                        id: tool_call
                            .id
                            .clone()
                            .unwrap_or_else(|| format!("toolu_{}", index)),
                        name: function.name.clone(),
                        input,
                    });
                }
            }
        }

        blocks
    } else {
        vec![]
    };

    let usage = openai_resp
        .usage
        .map(|u| AnthropicUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
        })
        .unwrap_or_default();

    AnthropicResponse {
        id: openai_resp.id,
        response_type: "message".to_string(),
        role: "assistant".to_string(),
        model: response_model,
        content,
        usage,
        stop_reason: openai_resp.choices.first().and_then(|c| {
            c.finish_reason.as_deref().map(|reason| match reason {
                "stop" => "end_turn".to_string(),
                "length" => "max_tokens".to_string(),
                "tool_calls" => "tool_use".to_string(),
                "content_filter" => "stop_sequence".to_string(),
                other => other.to_string(),
            })
        }),
    }
}

/// Translate an OpenAI streaming chunk to Anthropic streaming events.
fn translate_stream_chunk_to_anthropic(
    chunk: &OpenAIStreamChunk,
    is_first: bool,
) -> Vec<AnthropicStreamEvent> {
    let mut events = Vec::new();

    // Send message_start on first chunk
    if is_first {
        events.push(AnthropicStreamEvent {
            event_type: "message_start".to_string(),
            message: Some(serde_json::json!({
                "id": chunk.id,
                "type": "message",
                "role": "assistant",
                "model": chunk.model,
                "usage": null
            })),
            index: None,
            content_block: None,
            delta: None,
            usage: None,
            stop_reason: None,
        });

        // Start content block
        events.push(AnthropicStreamEvent {
            event_type: "content_block_start".to_string(),
            message: None,
            index: Some(0),
            content_block: Some(serde_json::json!({
                "type": "text",
                "text": ""
            })),
            delta: None,
            usage: None,
            stop_reason: None,
        });
    }

    // Handle content delta
    if let Some(choice) = chunk.choices.first() {
        // Handle reasoning content (for reasoning models)
        if let Some(ref reasoning) = choice.delta.reasoning_content {
            if !reasoning.is_empty() {
                events.push(AnthropicStreamEvent {
                    event_type: "content_block_delta".to_string(),
                    message: None,
                    index: Some(0),
                    content_block: None,
                    delta: Some(serde_json::json!({
                        "type": "thinking_delta",
                        "thinking": reasoning
                    })),
                    usage: None,
                    stop_reason: None,
                });
            }
        }

        // Handle regular content
        if let Some(ref content) = choice.delta.content {
            if !content.is_empty() {
                events.push(AnthropicStreamEvent {
                    event_type: "content_block_delta".to_string(),
                    message: None,
                    index: Some(0),
                    content_block: None,
                    delta: Some(serde_json::json!({
                        "type": "text_delta",
                        "text": content
                    })),
                    usage: None,
                    stop_reason: None,
                });
            }
        }

        // Handle tool call deltas.
        if let Some(tool_calls) = &choice.delta.tool_calls {
            for tool_call in tool_calls {
                let tool_index = tool_call.index + 1;
                if tool_call.id.is_some()
                    || tool_call
                        .function
                        .as_ref()
                        .and_then(|f| f.name.as_ref())
                        .is_some()
                {
                    events.push(AnthropicStreamEvent {
                        event_type: "content_block_start".to_string(),
                        message: None,
                        index: Some(tool_index),
                        content_block: Some(serde_json::json!({
                            "type": "tool_use",
                            "id": tool_call
                                .id
                                .clone()
                                .unwrap_or_else(|| format!("toolu_stream_{}", tool_index)),
                            "name": tool_call
                                .function
                                .as_ref()
                                .and_then(|f| f.name.as_ref())
                                .cloned()
                                .unwrap_or_default(),
                            "input": {}
                        })),
                        delta: None,
                        usage: None,
                        stop_reason: None,
                    });
                }

                if let Some(arguments) = tool_call
                    .function
                    .as_ref()
                    .and_then(|f| f.arguments.as_ref())
                    .filter(|a| !a.is_empty())
                {
                    events.push(AnthropicStreamEvent {
                        event_type: "content_block_delta".to_string(),
                        message: None,
                        index: Some(tool_index),
                        content_block: None,
                        delta: Some(serde_json::json!({
                            "type": "input_json_delta",
                            "partial_json": arguments
                        })),
                        usage: None,
                        stop_reason: None,
                    });
                }
            }
        }

        // Handle finish reason
        if choice.finish_reason.is_some() {
            events.push(AnthropicStreamEvent {
                event_type: "content_block_stop".to_string(),
                message: None,
                index: Some(0),
                content_block: None,
                delta: None,
                usage: None,
                stop_reason: None,
            });
        }
    }

    events
}

/// Create final Anthropic stream events (message_delta, message_stop)
fn create_stream_stop_events(usage: Option<AnthropicUsage>) -> Vec<AnthropicStreamEvent> {
    let mut events = Vec::new();

    let usage = usage.unwrap_or_default();

    events.push(AnthropicStreamEvent {
        event_type: "message_delta".to_string(),
        message: None,
        index: None,
        content_block: None,
        delta: Some(serde_json::json!({"stop_reason": "end_turn"})),
        usage: Some(usage.clone()),
        stop_reason: None,
    });

    events.push(AnthropicStreamEvent {
        event_type: "message_stop".to_string(),
        message: None,
        index: None,
        content_block: None,
        delta: None,
        usage: Some(usage),
        stop_reason: None,
    });

    events
}

/// Build the transformer chain for a given provider and model.
///
/// Combines provider-level transformers with any model-specific overrides.
fn build_transformer_chain(
    registry: &TransformerRegistry,
    provider: &crate::config::Provider,
    model: &str,
) -> TransformerChain {
    // Start with provider-level transformers
    let mut all_entries = provider.provider_transformers().to_vec();

    // Add model-specific transformers if configured
    if let Some(model_transformers) = provider.model_transformers(model) {
        all_entries.extend(model_transformers.to_vec());
    }

    registry.build_chain(&all_entries)
}

// ============================================================================
// Request Handler
// ============================================================================

/// Check if request contains [search] or [web] tags.
fn needs_web_search(request: &AnthropicRequest) -> bool {
    for msg in &request.messages {
        if let Some(text) = msg.content.as_str() {
            if text.contains("[search]") || text.contains("[web]") {
                return true;
            }
        }
    }
    false
}

/// Remove [search] and [web] tags from message content.
fn strip_search_tags(request: &mut AnthropicRequest) {
    for msg in &mut request.messages {
        if let Some(text) = msg.content.as_str() {
            let cleaned = text
                .replace("[search]", "")
                .replace("[web]", "")
                .trim()
                .to_string();
            msg.content = serde_json::Value::String(cleaned);
        }
    }
}

pub async fn handle_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AnthropicRequest>,
) -> Response {
    let start = std::time::Instant::now();
    let config = &state.config;
    let tiers = config.backend_tiers();

    let mut request = request;
    let mut ordered = state.ewma_tracker.sort_tiers_with_config(&tiers, config);

    // Check if the requested model explicitly targets a specific provider (e.g., "deepseek,deepseek-chat")
    // If so, route directly to that provider instead of cascading through tiers
    let requested_model = request.model.clone();
    if requested_model.contains(',') {
        // Explicit provider,model - find matching tier and prioritize it
        if let Some(pos) = ordered
            .iter()
            .position(|(tier, _)| tier == &requested_model)
        {
            // Move the requested tier to the front
            let target = ordered.remove(pos);
            ordered.insert(0, target);
            info!("Direct routing: {} moved to front", requested_model);
        } else {
            // Requested model not in tiers - try it directly as a single-tier request
            let tier_name = state
                .config
                .backend_abbreviation_with_config(&requested_model);
            ordered = vec![(requested_model.clone(), tier_name)];
            info!("Direct routing: {} (not in tier list)", requested_model);
        }
    }

    // Check for web search
    if state.config.router().web_search.enabled && needs_web_search(&request) {
        strip_search_tags(&mut request);
        if let Some(ref search_provider) = state.config.router().web_search.search_provider {
            // Prepend search provider as first tier
            ordered.insert(0, (search_provider.clone(), "search".to_string()));
            tracing::info!("Web search enabled, prepending {}", search_provider);
        }
    }

    // Detect frontend type from headers and request
    let body_json = serde_json::to_value(&request).unwrap_or_default();
    let frontend = detect_frontend(&headers, &body_json);
    info!(
        "Incoming request for model: {} (frontend: {:?})",
        request.model, frontend
    );

    // Serialize messages to JSON values once for pre-request token audit
    let msg_values: Vec<serde_json::Value> = request
        .messages
        .iter()
        .filter_map(|m| serde_json::to_value(m).ok())
        .collect();
    let tool_values: Option<Vec<serde_json::Value>> = request.tools.clone();

    // Try each tier with retries
    for (tier, tier_name) in ordered.iter() {
        if state.ratelimit_tracker.should_skip_tier(tier_name) {
            tracing::debug!(tier = %tier_name, "Skipping rate-limited tier");
            continue;
        }
        // Pre-request token audit: estimate input tokens before dispatching
        let local_estimate = record_pre_request_tokens(
            tier_name,
            &msg_values,
            request.system.as_ref(),
            tool_values.as_deref(),
        );

        let retry_config = config.get_tier_retry(tier_name);
        let max_retries = retry_config.max_retries;

        for attempt in 0..=max_retries {
            info!(
                "Trying {} ({}), attempt {}/{}",
                tier,
                tier_name,
                attempt + 1,
                max_retries + 1
            );

            // Override model with current tier
            request.model = tier.clone();

            // Start per-attempt latency timer for EWMA tracking
            let timer = AttemptTimer::start(&state.ewma_tracker, tier_name);

            match try_request(
                config,
                &state.transformer_registry,
                &request,
                tier,
                tier_name,
                local_estimate,
                state.ratelimit_tracker.clone(),
            )
            .await
            {
                Ok(response) => {
                    let attempt_duration = timer.finish_success();
                    let total_duration = start.elapsed().as_secs_f64();
                    record_request_with_frontend(tier_name, frontend);
                    record_request_duration_with_frontend(tier_name, total_duration, frontend);
                    sync_ewma_gauge(&state.ewma_tracker);
                    info!(
                        "Success on {} after {:.2}s (attempt {:.3}s)",
                        tier_name, total_duration, attempt_duration
                    );
                    return response;
                }
                Err(TryRequestError::RateLimited(retry_after)) => {
                    timer.finish_failure();
                    sync_ewma_gauge(&state.ewma_tracker);
                    warn!(
                        "Rate limited on {} attempt {} (retry-after: {:?})",
                        tier_name,
                        attempt + 1,
                        retry_after
                    );
                    record_failure(tier_name, "rate_limited");
                    record_rate_limit_hit(tier_name);
                    // Record 429 for backoff tracking
                    state.ratelimit_tracker.record_429(tier_name, retry_after);
                    record_rate_limit_backoff(tier_name);
                    // Skip remaining retries for this tier - move to next
                    break;
                }
                Err(TryRequestError::Other(e)) => {
                    timer.finish_failure();
                    sync_ewma_gauge(&state.ewma_tracker);
                    warn!("Failed {} attempt {}: {}", tier_name, attempt + 1, e);
                    record_failure(tier_name, "request_failed");

                    if attempt < max_retries {
                        // Get current EWMA for this tier for dynamic backoff scaling
                        let ewma = state.ewma_tracker.get_latency(tier_name).map(|(e, _)| e);
                        let backoff = retry_config.backoff_duration_with_ewma(attempt, ewma);
                        info!(
                            tier = tier_name,
                            attempt = attempt + 1,
                            backoff_ms = backoff.as_millis(),
                            ewma = ewma
                                .map(|e| format!("{:.3}s", e))
                                .unwrap_or_else(|| "N/A".to_string()),
                            "sleeping before retry"
                        );
                        tokio::time::sleep(backoff).await;
                    }
                }
            }
        }
    }

    // All tiers exhausted
    let total_attempts: usize = ordered
        .iter()
        .map(|(_, tier_name)| config.get_tier_retry(tier_name).max_retries + 1)
        .sum();
    error!("All tiers exhausted after {} tier(s)", ordered.len());
    let error_resp = ErrorResponse {
        error: "All backend tiers failed".to_string(),
        tier: "all".to_string(),
        attempts: total_attempts,
    };

    (StatusCode::SERVICE_UNAVAILABLE, Json(error_resp)).into_response()
}

// ============================================================================
// OpenAI-Compatible Endpoint
// ============================================================================

fn internal_request_to_anthropic_request(
    req: crate::frontend::InternalRequest,
) -> AnthropicRequest {
    AnthropicRequest {
        model: req.model,
        messages: req
            .messages
            .into_iter()
            .map(|m| Message {
                role: m.role,
                content: m.content,
                tool_call_id: m.tool_call_id,
            })
            .collect(),
        system: req.system,
        max_tokens: req.max_tokens,
        temperature: req.temperature,
        stream: req.stream,
        tools: req.tools.map(|tools| {
            tools
                .into_iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema.unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}))
                    })
                })
                .collect()
        }),
    }
}

fn anthropic_response_to_internal(
    response: AnthropicResponse,
) -> crate::frontend::InternalResponse {
    let content = response
        .content
        .into_iter()
        .map(|block| match block {
            AnthropicContentBlock::Text { text } => crate::frontend::ContentBlock::Text { text },
            AnthropicContentBlock::Thinking {
                thinking,
                signature,
            } => crate::frontend::ContentBlock::Thinking {
                thinking,
                signature: if signature.is_empty() {
                    None
                } else {
                    Some(signature)
                },
            },
            AnthropicContentBlock::ToolUse { id, name, input } => {
                crate::frontend::ContentBlock::ToolUse { id, name, input }
            }
        })
        .collect();

    crate::frontend::InternalResponse {
        id: response.id,
        response_type: response.response_type,
        role: response.role,
        model: response.model,
        content,
        stop_reason: response.stop_reason,
        usage: Some(crate::frontend::Usage {
            input_tokens: response.usage.input_tokens,
            output_tokens: response.usage.output_tokens,
            input_tokens_details: None,
        }),
        extra_data: None,
    }
}

fn parse_sse_frames(payload: &str) -> Vec<(Option<String>, String)> {
    let mut frames = Vec::new();
    let normalized = payload.replace("\r\n", "\n");

    for frame in normalized.split("\n\n") {
        if frame.trim().is_empty() {
            continue;
        }

        let mut event_type = None;
        let mut data_lines: Vec<String> = Vec::new();
        for line in frame.lines() {
            if let Some(rest) = line.strip_prefix("event:") {
                event_type = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                data_lines.push(rest.trim_start().to_string());
            }
        }

        if data_lines.is_empty() {
            continue;
        }
        frames.push((event_type, data_lines.join("\n")));
    }

    frames
}

fn decode_request_body(bytes: &[u8], headers: &HeaderMap) -> Result<Vec<u8>, String> {
    let content_encoding = headers
        .get(axum::http::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    if content_encoding.is_empty() || content_encoding == "identity" {
        return Ok(bytes.to_vec());
    }

    if content_encoding.contains("zstd") || content_encoding.contains("zst") {
        return zstd::stream::decode_all(std::io::Cursor::new(bytes))
            .map_err(|e| format!("Failed to decode zstd request body: {}", e));
    }

    Err(format!(
        "Unsupported content-encoding '{}'",
        content_encoding
    ))
}

fn parse_json_payload(bytes: &[u8]) -> Result<serde_json::Value, String> {
    if let Ok(value) = serde_json::from_slice(bytes) {
        return Ok(value);
    }

    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("Request body was empty".to_string());
    }

    // Some clients may double-encode JSON as a string payload.
    if let Ok(serde_json::Value::String(inner)) = serde_json::from_str::<serde_json::Value>(trimmed)
    {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&inner) {
            return Ok(value);
        }
    }

    // Defensive fallback: if there is envelope noise, parse the first JSON object span.
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if end > start {
            let candidate = &trimmed[start..=end];
            if let Ok(value) = serde_json::from_str(candidate) {
                return Ok(value);
            }
        }
    }

    let preview: String = trimmed.chars().take(200).collect();
    Err(format!(
        "Failed to parse request body as JSON (preview: {:?})",
        preview
    ))
}

async fn convert_anthropic_json_response_to_openai(response: Response) -> Response {
    let (mut parts, body) = response.into_parts();
    let body_bytes = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(err) => {
            error!("Failed to read Anthropic response body: {}", err);
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": "Failed to read upstream response"})),
            )
                .into_response();
        }
    };

    if parts.status != StatusCode::OK {
        // Normalize rate limit errors
        if parts.status == StatusCode::TOO_MANY_REQUESTS {
            if let Ok(mut error_json) = serde_json::from_slice::<serde_json::Value>(&body_bytes) {
                if let Some(error_obj) = error_json.get_mut("error").and_then(|e| e.as_object_mut())
                {
                    error_obj.insert(
                        "type".to_string(),
                        serde_json::Value::String("rate_limit_error".to_string()),
                    );
                    error_obj.insert(
                        "code".to_string(),
                        serde_json::Value::String("rate_limited".to_string()),
                    );

                    if let Some(retry_after) = parts
                        .headers
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<i64>().ok())
                    {
                        error_obj.insert("retry_after".to_string(), serde_json::json!(retry_after));
                    }
                }

                return Response::from_parts(
                    parts,
                    Body::from(serde_json::to_vec(&error_json).unwrap_or(body_bytes.to_vec())),
                );
            }
        }
        return Response::from_parts(parts, Body::from(body_bytes));
    }

    let anthropic_response: AnthropicResponse = match serde_json::from_slice(&body_bytes) {
        Ok(resp) => resp,
        Err(_) => {
            // If the payload is not Anthropic-shaped, pass through unchanged.
            return Response::from_parts(parts, Body::from(body_bytes));
        }
    };

    let internal_response = anthropic_response_to_internal(anthropic_response);
    let frontend = CodexFrontend::new();
    let serialized = match frontend.serialize_response(internal_response) {
        Ok(bytes) => bytes,
        Err(err) => {
            error!("Failed to serialize OpenAI response: {}", err);
            return Response::from_parts(parts, Body::from(body_bytes));
        }
    };

    parts.headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    Response::from_parts(parts, Body::from(serialized))
}

async fn convert_anthropic_stream_response_to_openai(response: Response) -> Response {
    let (mut parts, body) = response.into_parts();

    if parts.status != StatusCode::OK {
        // For error responses, read the whole body (it's likely small)
        let body_bytes = match to_bytes(body, usize::MAX).await {
            Ok(bytes) => bytes,
            Err(_) => return Response::from_parts(parts, Body::empty()),
        };

        // Normalize rate limit errors
        if parts.status == StatusCode::TOO_MANY_REQUESTS {
            if let Ok(mut error_json) = serde_json::from_slice::<serde_json::Value>(&body_bytes) {
                if let Some(error_obj) = error_json.get_mut("error").and_then(|e| e.as_object_mut())
                {
                    error_obj.insert(
                        "type".to_string(),
                        serde_json::Value::String("rate_limit_error".to_string()),
                    );
                    error_obj.insert(
                        "code".to_string(),
                        serde_json::Value::String("rate_limited".to_string()),
                    );

                    if let Some(retry_after) = parts
                        .headers
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<i64>().ok())
                    {
                        error_obj.insert("retry_after".to_string(), serde_json::json!(retry_after));
                    }
                }

                return Response::from_parts(
                    parts,
                    Body::from(serde_json::to_vec(&error_json).unwrap_or(body_bytes.to_vec())),
                );
            }
        }

        return Response::from_parts(parts, Body::from(body_bytes));
    }

    parts.headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/event-stream"),
    );

    // Create a channel for streaming response
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(100);

    tokio::spawn(async move {
        let mut stream = body.into_data_stream();
        let mut decoder = SseFrameDecoder::new();
        let transformer = AnthropicToOpenAiResponseTransformer;
        let mut sent_done = false;

        loop {
            tokio::select! {
                chunk = stream.next() => {
                    let Some(chunk_res) = chunk else { break; };
                    match chunk_res {
                        Ok(bytes) => {
                            for frame in decoder.push(&bytes) {
                                let data = frame.data;
                                let event_type = frame.event;

                                if data.trim() == "[DONE]" {
                                    if !sent_done {
                                        let _ = tx.send(Ok(Bytes::from("data: [DONE]\n\n"))).await;
                                        sent_done = true;
                                    }
                                    continue;
                                }

                                let mut event_json: serde_json::Value = match serde_json::from_str(&data) {
                                    Ok(value) => value,
                                    Err(_) => {
                                        // Pass through raw data if parsing fails
                                        let msg = format!("data: {}\n\n", data);
                                        let _ = tx.send(Ok(Bytes::from(msg))).await;
                                        continue;
                                    }
                                };

                                if event_json.get("type").is_none() {
                                    if let Some(t) = event_type.as_deref() {
                                        event_json["type"] = serde_json::Value::String(t.to_string());
                                    }
                                }

                                let transformed: serde_json::Value = match transformer.transform_response(event_json) {
                                    Ok(value) => value,
                                    Err(_) => {
                                        let msg = format!("data: {}\n\n", data);
                                        let _ = tx.send(Ok(Bytes::from(msg))).await;
                                        continue;
                                    }
                                };

                                let msg = format!("data: {}\n\n", serde_json::to_string(&transformed).unwrap_or_default());
                                if tx.send(Ok(Bytes::from(msg))).await.is_err() {
                                    return; // Receiver closed
                                }

                                if event_type.as_deref() == Some("message_stop") && !sent_done {
                                    let _ = tx.send(Ok(Bytes::from("data: [DONE]\n\n"))).await;
                                    sent_done = true;
                                }
                            }
                        }
                        Err(e) => {
                            error!("Stream read error: {}", e);
                            break;
                        }
                    }
                }
                _ = tx.closed() => break,
            }
        }

        if !sent_done {
            let _ = tx.send(Ok(Bytes::from("data: [DONE]\n\n"))).await;
        }
    });

    Response::from_parts(parts, Body::from_stream(ReceiverStream::new(rx)))
}

fn map_openai_usage_to_responses_usage(usage: &serde_json::Value) -> serde_json::Value {
    let prompt_tokens = usage
        .get("prompt_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion_tokens = usage
        .get("completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(prompt_tokens + completion_tokens);
    let cached_tokens = usage
        .get("prompt_tokens_details")
        .and_then(|v| v.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let reasoning_tokens = usage
        .get("completion_tokens_details")
        .and_then(|v| v.get("reasoning_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    serde_json::json!({
        "input_tokens": prompt_tokens,
        "input_tokens_details": {
            "cached_tokens": cached_tokens
        },
        "output_tokens": completion_tokens,
        "output_tokens_details": {
            "reasoning_tokens": reasoning_tokens
        },
        "total_tokens": total_tokens
    })
}

fn openai_chat_completion_to_responses_json(openai: &serde_json::Value) -> serde_json::Value {
    let response_id = openai
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("resp_unknown");
    let created_at = openai
        .get("created")
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64
        });
    let model = openai
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let mut output_items = Vec::new();
    if let Some(choice) = openai
        .get("choices")
        .and_then(|choices| choices.as_array())
        .and_then(|choices| choices.first())
    {
        if let Some(message) = choice.get("message") {
            let mut content_blocks = Vec::new();
            if let Some(content) = message.get("content") {
                match content {
                    serde_json::Value::String(text) if !text.is_empty() => {
                        content_blocks.push(serde_json::json!({
                            "type": "output_text",
                            "text": text
                        }));
                    }
                    serde_json::Value::Array(items) => {
                        for item in items {
                            if item.get("type").and_then(|v| v.as_str()) == Some("text") {
                                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                                    content_blocks.push(serde_json::json!({
                                        "type": "output_text",
                                        "text": text
                                    }));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }

            output_items.push(serde_json::json!({
                "id": format!("msg_{}", response_id),
                "type": "message",
                "role": "assistant",
                "content": content_blocks
            }));

            if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
                for tool_call in tool_calls {
                    let call_id = tool_call
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("call_unknown");
                    let name = tool_call
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool");
                    let arguments = tool_call
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("{}");

                    output_items.push(serde_json::json!({
                        "id": call_id,
                        "type": "function_call",
                        "call_id": call_id,
                        "name": name,
                        "arguments": arguments
                    }));
                }
            }
        }
    }

    let usage = openai
        .get("usage")
        .map(map_openai_usage_to_responses_usage)
        .unwrap_or_else(|| map_openai_usage_to_responses_usage(&serde_json::json!({})));

    serde_json::json!({
        "id": response_id,
        "object": "response",
        "created_at": created_at,
        "status": "completed",
        "model": model,
        "output": output_items,
        "usage": usage
    })
}

fn responses_content_to_openai_content(content: &serde_json::Value) -> serde_json::Value {
    match content {
        serde_json::Value::Array(items) => {
            let mut blocks = Vec::new();
            for item in items {
                let block_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match block_type {
                    "input_text" | "output_text" => {
                        if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                            blocks.push(serde_json::json!({"type": "text", "text": text}));
                        }
                    }
                    "input_image" => {
                        if let Some(image_url) = item.get("image_url").and_then(|v| v.as_str()) {
                            blocks.push(serde_json::json!({
                                "type": "image_url",
                                "image_url": {"url": image_url}
                            }));
                        }
                    }
                    _ => {
                        if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                            blocks.push(serde_json::json!({"type": "text", "text": text}));
                        }
                    }
                }
            }
            if blocks.len() == 1 && blocks[0].get("type").and_then(|v| v.as_str()) == Some("text") {
                serde_json::Value::String(
                    blocks[0]
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                )
            } else {
                serde_json::Value::Array(blocks)
            }
        }
        serde_json::Value::String(text) => serde_json::Value::String(text.clone()),
        _ => serde_json::Value::String(content.to_string()),
    }
}

fn normalize_tool_output(output: &serde_json::Value) -> String {
    match output {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => {
            let mut combined = String::new();
            for item in items {
                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                    combined.push_str(text);
                }
            }
            if combined.is_empty() {
                output.to_string()
            } else {
                combined
            }
        }
        _ => output.to_string(),
    }
}

fn normalize_responses_message_role(role: &str) -> &str {
    match role {
        // OpenAI Responses API `developer` role should be treated as `system`
        // for providers that only accept classic OpenAI chat roles.
        "developer" => "system",
        _ => role,
    }
}

fn responses_request_to_openai_chat_request(
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "responses request requires 'model'".to_string())?;

    let mut messages: Vec<serde_json::Value> = Vec::new();

    if let Some(instructions) = body.get("instructions").and_then(|v| v.as_str()) {
        if !instructions.is_empty() {
            messages.push(serde_json::json!({
                "role": "system",
                "content": instructions
            }));
        }
    }

    if let Some(input_items) = body.get("input").and_then(|v| v.as_array()) {
        for item in input_items {
            let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match item_type {
                "message" => {
                    let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("user");
                    let role = normalize_responses_message_role(role);
                    let content = item
                        .get("content")
                        .map(responses_content_to_openai_content)
                        .unwrap_or_else(|| serde_json::Value::String(String::new()));
                    messages.push(serde_json::json!({
                        "role": role,
                        "content": content
                    }));
                }
                "function_call_output" | "custom_tool_call_output" => {
                    let call_id = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("call_unknown");
                    let output = item
                        .get("output")
                        .map(normalize_tool_output)
                        .unwrap_or_default();
                    messages.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": output
                    }));
                }
                "function_call" | "custom_tool_call" | "local_shell_call" => {
                    let call_id = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("id").and_then(|v| v.as_str()))
                        .unwrap_or("call_unknown");
                    let name = match item_type {
                        "function_call" => item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("function_call"),
                        "custom_tool_call" => item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("custom_tool_call"),
                        _ => "local_shell",
                    };

                    let arguments = match item_type {
                        "function_call" => item
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| {
                                item.get("arguments")
                                    .cloned()
                                    .unwrap_or_else(|| serde_json::json!({}))
                                    .to_string()
                            }),
                        "custom_tool_call" => item
                            .get("input")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| {
                                item.get("input")
                                    .cloned()
                                    .unwrap_or_else(|| serde_json::json!({}))
                                    .to_string()
                            }),
                        _ => item
                            .get("action")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({}))
                            .to_string(),
                    };

                    messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": "",
                        "tool_calls": [{
                            "id": call_id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": arguments
                            }
                        }]
                    }));
                }
                _ => {}
            }
        }
    }

    let mut request = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": body.get("stream").and_then(|v| v.as_bool()).unwrap_or(true)
    });

    if let Some(tools) = body.get("tools").cloned() {
        request["tools"] = tools;
    }
    if let Some(tool_choice) = body.get("tool_choice").cloned() {
        request["tool_choice"] = tool_choice;
    }
    if let Some(temperature) = body.get("temperature").cloned() {
        request["temperature"] = temperature;
    }
    if let Some(top_p) = body.get("top_p").cloned() {
        request["top_p"] = top_p;
    }
    if let Some(max_tokens) = body.get("max_output_tokens").cloned() {
        request["max_tokens"] = max_tokens;
    }

    Ok(request)
}

async fn convert_openai_json_response_to_responses(response: Response) -> Response {
    let (mut parts, body) = response.into_parts();
    let body_bytes = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(err) => {
            error!("Failed to read OpenAI response: {}", err);
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": "Failed to read upstream response"})),
            )
                .into_response();
        }
    };

    let openai_json: serde_json::Value = match serde_json::from_slice(&body_bytes) {
        Ok(value) => value,
        Err(_) => return Response::from_parts(parts, Body::from(body_bytes)),
    };

    if parts.status != StatusCode::OK {
        // Check if this is a rate limit error (429)
        let is_rate_limit = parts.status == StatusCode::TOO_MANY_REQUESTS;
        let retry_after = parts
            .headers
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());

        let error_message = openai_json
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("upstream request failed");

        let mut error_obj = serde_json::json!({
            "message": error_message
        });

        if is_rate_limit {
            error_obj["code"] = serde_json::Value::String("rate_limited".to_string());
            if let Some(retry_after_secs) = retry_after {
                error_obj["retry_after"] = serde_json::json!(retry_after_secs);
            }
        }

        let failed = serde_json::json!({
            "id": "resp_failed",
            "object": "response",
            "status": "failed",
            "error": error_obj
        });

        // Preserve the original status code and retry-after header for rate limits
        if is_rate_limit {
            if let Some(retry_after_secs) = retry_after {
                parts.headers.insert(
                    "retry-after",
                    axum::http::HeaderValue::from_str(&retry_after_secs.to_string())
                        .unwrap_or(axum::http::HeaderValue::from_static("0")),
                );
            }
        }

        parts.headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        return Response::from_parts(parts, Body::from(failed.to_string()));
    }

    let responses_json = openai_chat_completion_to_responses_json(&openai_json);
    parts.headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    Response::from_parts(parts, Body::from(responses_json.to_string()))
}

async fn convert_openai_stream_response_to_responses(response: Response) -> Response {
    let (mut parts, body) = response.into_parts();
    let body_bytes = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(err) => {
            error!("Failed to read OpenAI stream: {}", err);
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": "Failed to read upstream stream"})),
            )
                .into_response();
        }
    };

    let mut output = String::new();
    let payload = String::from_utf8_lossy(&body_bytes);

    if parts.status != StatusCode::OK {
        // Check if this is a rate limit error (429)
        let is_rate_limit = parts.status == StatusCode::TOO_MANY_REQUESTS;
        let retry_after = parts
            .headers
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());

        let error_text = String::from_utf8_lossy(&body_bytes).to_string();

        let mut error_obj = serde_json::json!({
            "message": error_text
        });

        if is_rate_limit {
            error_obj["code"] = serde_json::Value::String("rate_limited".to_string());
            if let Some(retry_after_secs) = retry_after {
                error_obj["retry_after"] = serde_json::json!(retry_after_secs);
            }
        } else {
            error_obj["code"] = serde_json::Value::String("upstream_error".to_string());
        }

        let failed_event = serde_json::json!({
            "type": "response.failed",
            "response": {
                "id": "resp_failed",
                "object": "response",
                "status": "failed",
                "error": error_obj
            }
        });
        output.push_str("event: response.failed\ndata: ");
        output.push_str(&failed_event.to_string());
        output.push_str("\n\n");
        parts.status = StatusCode::OK;
        parts.headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("text/event-stream"),
        );
        return Response::from_parts(parts, Body::from(output));
    }

    #[derive(Default)]
    struct ToolAccum {
        id: String,
        name: String,
        arguments: String,
        added: bool,
    }

    let mut response_id = "resp_stream".to_string();
    let mut created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let mut model = "unknown".to_string();
    let mut created_sent = false;
    let mut message_item_added = false;
    let mut message_text = String::new();
    let mut reasoning_text = String::new();
    let mut tools: std::collections::BTreeMap<usize, ToolAccum> = std::collections::BTreeMap::new();
    let mut usage = map_openai_usage_to_responses_usage(&serde_json::json!({}));

    for (_event_type, data) in parse_sse_frames(&payload) {
        if data.trim() == "[DONE]" {
            break;
        }

        let chunk: serde_json::Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if let Some(id) = chunk.get("id").and_then(|v| v.as_str()) {
            response_id = id.to_string();
        }
        if let Some(ts) = chunk.get("created").and_then(|v| v.as_i64()) {
            created_at = ts;
        }
        if let Some(m) = chunk.get("model").and_then(|v| v.as_str()) {
            model = m.to_string();
        }

        if !created_sent {
            let created_event = serde_json::json!({
                "type": "response.created",
                "response": {
                    "id": response_id,
                    "object": "response",
                    "created_at": created_at,
                    "status": "in_progress",
                    "model": model
                }
            });
            output.push_str("event: response.created\ndata: ");
            output.push_str(&created_event.to_string());
            output.push_str("\n\n");
            created_sent = true;
        }

        if let Some(u) = chunk.get("usage") {
            usage = map_openai_usage_to_responses_usage(u);
        }

        let choice = chunk
            .get("choices")
            .and_then(|v| v.as_array())
            .and_then(|v| v.first());
        let Some(choice) = choice else {
            continue;
        };

        let delta = choice.get("delta").cloned().unwrap_or_default();

        if delta
            .get("role")
            .and_then(|v| v.as_str())
            .is_some_and(|r| r == "assistant")
            && !message_item_added
        {
            let added = serde_json::json!({
                "type": "response.output_item.added",
                "item": {
                    "id": format!("msg_{}", response_id),
                    "type": "message",
                    "role": "assistant",
                    "content": []
                }
            });
            output.push_str("event: response.output_item.added\ndata: ");
            output.push_str(&added.to_string());
            output.push_str("\n\n");
            message_item_added = true;
        }

        if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
            if !message_item_added {
                let added = serde_json::json!({
                    "type": "response.output_item.added",
                    "item": {
                        "id": format!("msg_{}", response_id),
                        "type": "message",
                        "role": "assistant",
                        "content": []
                    }
                });
                output.push_str("event: response.output_item.added\ndata: ");
                output.push_str(&added.to_string());
                output.push_str("\n\n");
                message_item_added = true;
            }

            message_text.push_str(text);
            let delta_event = serde_json::json!({
                "type": "response.output_text.delta",
                "delta": text
            });
            output.push_str("event: response.output_text.delta\ndata: ");
            output.push_str(&delta_event.to_string());
            output.push_str("\n\n");
        }

        if let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
            reasoning_text.push_str(reasoning);
            let delta_event = serde_json::json!({
                "type": "response.reasoning_text.delta",
                "delta": reasoning,
                "content_index": 0
            });
            output.push_str("event: response.reasoning_text.delta\ndata: ");
            output.push_str(&delta_event.to_string());
            output.push_str("\n\n");
        }

        if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tool_call in tool_calls {
                let index = tool_call.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let entry = tools.entry(index).or_default();

                if let Some(id) = tool_call.get("id").and_then(|v| v.as_str()) {
                    entry.id = id.to_string();
                }
                if let Some(name) = tool_call
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|v| v.as_str())
                {
                    entry.name = name.to_string();
                }
                if let Some(args) = tool_call
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|v| v.as_str())
                {
                    entry.arguments.push_str(args);
                }

                if !entry.added && (!entry.id.is_empty() || !entry.name.is_empty()) {
                    if entry.id.is_empty() {
                        entry.id = format!("call_{}", index);
                    }
                    if entry.name.is_empty() {
                        entry.name = "tool".to_string();
                    }
                    let added = serde_json::json!({
                        "type": "response.output_item.added",
                        "item": {
                            "id": entry.id,
                            "type": "function_call",
                            "call_id": entry.id,
                            "name": entry.name,
                            "arguments": entry.arguments
                        }
                    });
                    output.push_str("event: response.output_item.added\ndata: ");
                    output.push_str(&added.to_string());
                    output.push_str("\n\n");
                    entry.added = true;
                }
            }
        }
    }

    let mut output_items = Vec::new();

    if message_item_added {
        let mut message_content = Vec::new();
        if !reasoning_text.is_empty() {
            message_content.push(serde_json::json!({
                "type": "output_text",
                "text": reasoning_text
            }));
        }
        if !message_text.is_empty() {
            message_content.push(serde_json::json!({
                "type": "output_text",
                "text": message_text
            }));
        }
        let message_item = serde_json::json!({
            "id": format!("msg_{}", response_id),
            "type": "message",
            "role": "assistant",
            "content": message_content
        });
        output_items.push(message_item.clone());
        let done = serde_json::json!({
            "type": "response.output_item.done",
            "item": message_item
        });
        output.push_str("event: response.output_item.done\ndata: ");
        output.push_str(&done.to_string());
        output.push_str("\n\n");
    }

    for tool in tools.values() {
        let call_id = if tool.id.is_empty() {
            "call_unknown"
        } else {
            &tool.id
        };
        let name = if tool.name.is_empty() {
            "tool"
        } else {
            &tool.name
        };
        let item = serde_json::json!({
            "id": call_id,
            "type": "function_call",
            "call_id": call_id,
            "name": name,
            "arguments": tool.arguments
        });
        output_items.push(item.clone());
        let done = serde_json::json!({
            "type": "response.output_item.done",
            "item": item
        });
        output.push_str("event: response.output_item.done\ndata: ");
        output.push_str(&done.to_string());
        output.push_str("\n\n");
    }

    let completed = serde_json::json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "object": "response",
            "created_at": created_at,
            "status": "completed",
            "model": model,
            "output": output_items,
            "usage": usage
        }
    });
    output.push_str("event: response.completed\ndata: ");
    output.push_str(&completed.to_string());
    output.push_str("\n\n");

    parts.headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/event-stream"),
    );
    parts.status = StatusCode::OK;
    Response::from_parts(parts, Body::from(output))
}

/// Handle OpenAI-format chat completion requests.
///
/// Converts to Anthropic format internally, processes the request,
/// then converts the response back to OpenAI format.
pub async fn handle_chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request_body): Json<serde_json::Value>,
) -> Response {
    let frontend = CodexFrontend::new();
    let internal_request = match frontend.parse_request(request_body) {
        Ok(req) => req,
        Err(e) => {
            error!("Failed to parse OpenAI request: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("Invalid request: {}", e)})),
            )
                .into_response();
        }
    };
    let stream_requested = internal_request.stream.unwrap_or(false);
    let anthropic_request = internal_request_to_anthropic_request(internal_request);
    let response = handle_messages(State(state), headers, Json(anthropic_request)).await;

    if stream_requested {
        convert_anthropic_stream_response_to_openai(response).await
    } else {
        convert_anthropic_json_response_to_openai(response).await
    }
}

/// Handle OpenAI Responses API requests.
pub async fn handle_responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let body_bytes = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": {
                        "message": format!("Failed to read request body: {}", err)
                    }
                })),
            )
                .into_response();
        }
    };

    let decoded = match decode_request_body(&body_bytes, &headers) {
        Ok(bytes) => bytes,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": {
                        "message": err
                    }
                })),
            )
                .into_response();
        }
    };

    let request_body = match parse_json_payload(&decoded) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": {
                        "message": err
                    }
                })),
            )
                .into_response();
        }
    };

    let stream_requested = request_body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let openai_chat_request = match responses_request_to_openai_chat_request(&request_body) {
        Ok(request) => request,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": {
                        "message": err
                    }
                })),
            )
                .into_response();
        }
    };

    let openai_response =
        handle_chat_completions(State(state), headers, Json(openai_chat_request)).await;

    if stream_requested {
        convert_openai_stream_response_to_responses(openai_response).await
    } else {
        convert_openai_json_response_to_responses(openai_response).await
    }
}

// ============================================================================
// Preset Handlers
// ============================================================================

/// List all configured presets.
pub async fn list_presets(State(state): State<AppState>) -> impl IntoResponse {
    let presets: Vec<_> = state
        .config
        .presets
        .iter()
        .map(|(name, cfg)| {
            serde_json::json!({
                "name": name,
                "route": cfg.route,
                "max_tokens": cfg.max_tokens,
                "temperature": cfg.temperature,
            })
        })
        .collect();
    Json(presets)
}

/// List available models in OpenAI-compatible format.
///
/// Includes both explicit route IDs (`provider,model`) and raw model IDs.
/// This is required by Codex/OpenAI clients that call `GET /v1/models`
/// before first request dispatch.
pub async fn list_models(State(state): State<AppState>) -> impl IntoResponse {
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let mut seen = BTreeSet::new();
    let mut data = Vec::new();

    for provider in state.config.providers() {
        for model in &provider.models {
            let ids = [format!("{},{}", provider.name, model), model.to_string()];
            for id in ids {
                if !seen.insert(id.clone()) {
                    continue;
                }
                data.push(serde_json::json!({
                    "id": id,
                    "object": "model",
                    "created": created,
                    "owned_by": provider.name,
                }));
            }
        }
    }

    Json(serde_json::json!({
        "object": "list",
        "data": data,
        // Codex currently expects a `models` field in `/v1/models` responses.
        // Keep this present for compatibility even when rich model metadata is unavailable.
        "models": []
    }))
}

/// Handle messages via a named preset.
pub async fn handle_preset_messages(
    State(state): State<AppState>,
    Path(preset_name): Path<String>,
    Json(mut request): Json<AnthropicRequest>,
) -> Response {
    let preset = match state.config.get_preset(&preset_name) {
        Some(p) => p,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": format!("Preset '{}' not found", preset_name)})),
            )
                .into_response();
        }
    };

    // Apply preset overrides
    if let Some(mt) = preset.max_tokens {
        request.max_tokens = Some(mt);
    }
    if let Some(temp) = preset.temperature {
        request.temperature = Some(temp);
    }

    // Force route to preset's tier
    request.model = preset.route.clone();

    // Delegate to normal handler
    handle_messages(State(state), HeaderMap::new(), Json(request)).await
}

async fn try_request(
    config: &Config,
    registry: &TransformerRegistry,
    request: &AnthropicRequest,
    tier: &str,
    tier_name: &str,
    local_estimate: u64,
    ratelimit_tracker: Arc<RateLimitTracker>,
) -> Result<Response, TryRequestError> {
    let provider = config.resolve_provider(tier).ok_or_else(|| {
        TryRequestError::Other(anyhow::anyhow!("Provider not found for tier: {}", tier))
    })?;

    // Build transformer chain from provider config
    let chain = build_transformer_chain(registry, provider, tier.split(',').nth(1).unwrap_or(tier));

    // Extract the actual model name from the tier (format: "provider,model")
    let model_name = tier.split(',').nth(1).unwrap_or(tier);

    // Apply request transformers if chain is not empty
    let transformed_request = if chain.is_empty() {
        serde_json::to_value(request).map_err(|e| TryRequestError::Other(e.into()))?
    } else {
        let req_value =
            serde_json::to_value(request).map_err(|e| TryRequestError::Other(e.into()))?;
        chain
            .apply_request(req_value)
            .map_err(TryRequestError::Other)?
    };

    match provider.protocol {
        ProviderProtocol::Openai => {
            try_request_via_openai_protocol(
                config,
                provider,
                TryRequestProtocolArgs {
                    transformed_request,
                    model_name,
                    tier_name,
                    local_estimate,
                    ratelimit_tracker,
                    chain,
                },
            )
            .await
        }
        ProviderProtocol::Anthropic => {
            try_request_via_anthropic_protocol(
                config,
                provider,
                TryRequestProtocolArgs {
                    transformed_request,
                    model_name,
                    tier_name,
                    local_estimate,
                    ratelimit_tracker,
                    chain,
                },
            )
            .await
        }
    }
}

struct TryRequestProtocolArgs<'a> {
    transformed_request: serde_json::Value,
    model_name: &'a str,
    tier_name: &'a str,
    local_estimate: u64,
    ratelimit_tracker: Arc<RateLimitTracker>,
    chain: TransformerChain,
}

const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

fn provider_endpoint_url(provider: &crate::config::Provider, endpoint: &str) -> String {
    let base = provider.api_base_url.trim_end_matches('/');
    let endpoint = endpoint.trim_start_matches('/');
    if base.ends_with(endpoint) {
        base.to_string()
    } else {
        format!("{}/{}", base, endpoint)
    }
}

fn provider_openai_chat_completions_url(provider: &crate::config::Provider) -> String {
    provider_endpoint_url(provider, "chat/completions")
}

fn provider_anthropic_messages_url(provider: &crate::config::Provider) -> String {
    provider_endpoint_url(provider, "messages")
}

fn reqwest_status_to_axum(status: reqwest::StatusCode) -> StatusCode {
    StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY)
}

fn insert_ccr_tier_header(response: &mut Response, tier_name: &str) {
    response.headers_mut().insert(
        "x-ccr-tier",
        tier_name
            .parse()
            .unwrap_or(axum::http::HeaderValue::from_static("unknown")),
    );
}

fn build_openai_headers(
    provider: &crate::config::Provider,
) -> Result<reqwest::header::HeaderMap, TryRequestError> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "Authorization",
        format!("Bearer {}", provider.api_key).parse().map_err(
            |e: reqwest::header::InvalidHeaderValue| {
                TryRequestError::Other(anyhow::anyhow!("{}", e))
            },
        )?,
    );
    headers.insert(
        "Content-Type",
        "application/json"
            .parse()
            .map_err(|e: reqwest::header::InvalidHeaderValue| {
                TryRequestError::Other(anyhow::anyhow!("{}", e))
            })?,
    );

    // OpenRouter attribution headers for usage tracking
    // See: https://openrouter.ai/docs/api-reference/overview
    if provider.name.to_lowercase() == "openrouter"
        || provider.api_base_url.contains("openrouter.ai")
    {
        headers.insert(
            "HTTP-Referer",
            "https://github.com/RESMP-DEV/ccr-rust".parse().map_err(
                |e: reqwest::header::InvalidHeaderValue| {
                    TryRequestError::Other(anyhow::anyhow!("{}", e))
                },
            )?,
        );
        headers.insert(
            "X-Title",
            "ccr-rust"
                .parse()
                .map_err(|e: reqwest::header::InvalidHeaderValue| {
                    TryRequestError::Other(anyhow::anyhow!("{}", e))
                })?,
        );
    }

    Ok(headers)
}

fn build_anthropic_headers(
    provider: &crate::config::Provider,
) -> Result<reqwest::header::HeaderMap, TryRequestError> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-api-key",
        provider
            .api_key
            .parse()
            .map_err(|e: reqwest::header::InvalidHeaderValue| {
                TryRequestError::Other(anyhow::anyhow!("{}", e))
            })?,
    );
    headers.insert(
        "anthropic-version",
        provider
            .anthropic_version
            .as_deref()
            .unwrap_or(DEFAULT_ANTHROPIC_VERSION)
            .parse()
            .map_err(|e: reqwest::header::InvalidHeaderValue| {
                TryRequestError::Other(anyhow::anyhow!("{}", e))
            })?,
    );
    headers.insert(
        "Content-Type",
        "application/json"
            .parse()
            .map_err(|e: reqwest::header::InvalidHeaderValue| {
                TryRequestError::Other(anyhow::anyhow!("{}", e))
            })?,
    );
    Ok(headers)
}

async fn try_request_via_openai_protocol(
    config: &Config,
    provider: &crate::config::Provider,
    args: TryRequestProtocolArgs<'_>,
) -> Result<Response, TryRequestError> {
    let TryRequestProtocolArgs {
        transformed_request,
        model_name,
        tier_name,
        local_estimate,
        ratelimit_tracker,
        chain,
    } = args;

    let url = provider_openai_chat_completions_url(provider);
    let headers = build_openai_headers(provider)?;
    trace!(tier = tier_name, model = model_name, url = %url, "dispatching OpenAI-compatible upstream request");

    // Deserialize back to AnthropicRequest for translation.
    let request: AnthropicRequest = serde_json::from_value(transformed_request)
        .map_err(|e| TryRequestError::Other(e.into()))?;

    // Translate Anthropic request to OpenAI format.
    let openai_request = translate_request_anthropic_to_openai(&request, model_name);

    let resp = config
        .http_client()
        .post(&url)
        .headers(headers)
        .json(&openai_request)
        .send()
        .await
        .map_err(|e| TryRequestError::Other(e.into()))?;

    if !resp.status().is_success() {
        let status = resp.status();

        // For 429 rate limit, pass through the response to allow proper error handling upstream
        // This allows the Responses API to return structured rate limit errors to clients
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            // Parse Retry-After header if present
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(std::time::Duration::from_secs);

            // Record the rate limit hit for tracking
            record_rate_limit_hit(tier_name);
            ratelimit_tracker.record_429(tier_name, retry_after);
            record_rate_limit_backoff(tier_name);

            // Pass through the 429 response as-is so it can be handled by response converters
            let mut builder =
                axum::response::Response::builder().status(reqwest_status_to_axum(status));

            // Copy headers
            for (key, value) in resp.headers() {
                if let Ok(name) = axum::http::HeaderName::from_bytes(key.as_str().as_bytes()) {
                    if let Ok(val) = axum::http::HeaderValue::from_bytes(value.as_bytes()) {
                        builder = builder.header(name, val);
                    }
                }
            }

            // Insert x-ccr-tier header
            builder = builder.header("x-ccr-tier", tier_name);

            let body_bytes = resp
                .bytes()
                .await
                .map_err(|e| TryRequestError::Other(e.into()))?;

            // Normalize the error response body to include required fields like 'code': 'rate_limited'
            let normalized_body = if let Ok(mut error_json) =
                serde_json::from_slice::<serde_json::Value>(&body_bytes)
            {
                if let Some(error_obj) = error_json.get_mut("error").and_then(|e| e.as_object_mut())
                {
                    error_obj.insert(
                        "type".to_string(),
                        serde_json::Value::String("rate_limit_error".to_string()),
                    );
                    error_obj.insert(
                        "code".to_string(),
                        serde_json::Value::String("rate_limited".to_string()),
                    );
                    if let Some(retry_after_secs) = retry_after {
                        error_obj.insert(
                            "retry_after".to_string(),
                            serde_json::json!(retry_after_secs.as_secs()),
                        );
                    }
                }
                serde_json::to_vec(&error_json).unwrap_or(body_bytes.to_vec())
            } else {
                body_bytes.to_vec()
            };

            return builder.body(Body::from(normalized_body)).map_err(|e| {
                TryRequestError::Other(anyhow::anyhow!("Failed to build response: {}", e))
            });
        }

        let body = resp
            .text()
            .await
            .map_err(|e| TryRequestError::Other(e.into()))?;
        return Err(TryRequestError::Other(anyhow::anyhow!(
            "Provider returned {} from {}: {}",
            status,
            url,
            body
        )));
    }

    // Handle streaming vs non-streaming.
    if request.stream.unwrap_or(false) {
        // For streaming, we need to translate OpenAI SSE events to Anthropic SSE.
        let rate_limit_info = extract_rate_limit_headers(&resp);
        let ctx = StreamVerifyCtx {
            tier_name: tier_name.to_string(),
            local_estimate,
            ratelimit_tracker: Some(ratelimit_tracker.clone()),
            rate_limit_info: Some(rate_limit_info),
        };
        Ok(
            stream_response_translated(
                resp,
                config.sse_buffer_size(),
                Some(ctx),
                model_name,
                chain,
            )
            .await,
        )
    } else {
        // Extract rate limit headers for non-streaming.
        let rate_limit_info = extract_rate_limit_headers(&resp);
        ratelimit_tracker.record_success(tier_name, rate_limit_info.0, rate_limit_info.1);

        // For non-streaming, translate the full response.
        let body = resp
            .bytes()
            .await
            .map_err(|e| TryRequestError::Other(e.into()))?;

        // Try to parse as OpenAI response and translate.
        if let Ok(openai_resp) = serde_json::from_slice::<OpenAIResponse>(&body) {
            // Record usage from the response.
            if let Some(ref usage) = openai_resp.usage {
                record_usage(
                    tier_name,
                    usage.prompt_tokens,
                    usage.completion_tokens,
                    0, // OpenAI doesn't have cache fields in the same way
                    0,
                );
                verify_token_usage(tier_name, local_estimate, usage.prompt_tokens);
            }

            // Translate to Anthropic format.
            let anthropic_resp = translate_response_openai_to_anthropic(openai_resp, model_name);

            // Apply response transformers if chain is not empty.
            let final_resp = if chain.is_empty() {
                anthropic_resp
            } else {
                let resp_value = serde_json::to_value(&anthropic_resp)
                    .map_err(|e| TryRequestError::Other(e.into()))?;
                let transformed = chain
                    .apply_response(resp_value)
                    .map_err(TryRequestError::Other)?;
                serde_json::from_value::<AnthropicResponse>(transformed).unwrap_or(anthropic_resp)
            };

            let response_body =
                serde_json::to_vec(&final_resp).map_err(|e| TryRequestError::Other(e.into()))?;

            let mut response = (StatusCode::OK, response_body).into_response();
            insert_ccr_tier_header(&mut response, tier_name);
            return Ok(response);
        }

        // Fallback: pass through original response if translation fails.
        let mut response = (StatusCode::OK, body).into_response();
        insert_ccr_tier_header(&mut response, tier_name);
        Ok(response)
    }
}

async fn try_request_via_anthropic_protocol(
    config: &Config,
    provider: &crate::config::Provider,
    args: TryRequestProtocolArgs<'_>,
) -> Result<Response, TryRequestError> {
    let TryRequestProtocolArgs {
        transformed_request,
        model_name,
        tier_name,
        local_estimate,
        ratelimit_tracker,
        chain,
    } = args;

    let url = provider_anthropic_messages_url(provider);
    let headers = build_anthropic_headers(provider)?;
    trace!(tier = tier_name, model = model_name, url = %url, "dispatching Anthropic-compatible upstream request");

    let request: AnthropicRequest = serde_json::from_value(transformed_request)
        .map_err(|e| TryRequestError::Other(e.into()))?;

    // Canonicalize request shape for Anthropic-compatible providers.
    //
    // Codex/Responses inputs can introduce OpenAI-style message/tool structures
    // (e.g., tool call/result shapes). Round-trip through OpenAI and run the
    // dedicated OpenAI->Anthropic transformer to normalize roles/content/tools.
    let openai_request = translate_request_anthropic_to_openai(&request, model_name);
    let openai_request_value =
        serde_json::to_value(openai_request).map_err(|e| TryRequestError::Other(e.into()))?;
    let mut normalized_request_value = OpenAiToAnthropicTransformer
        .transform_request(openai_request_value)
        .map_err(TryRequestError::Other)?;

    if let Some(obj) = normalized_request_value.as_object_mut() {
        obj.insert(
            "model".to_string(),
            serde_json::Value::String(model_name.to_string()),
        );
    }

    let request: AnthropicRequest = serde_json::from_value(normalized_request_value)
        .map_err(|e| TryRequestError::Other(e.into()))?;

    let resp = config
        .http_client()
        .post(&url)
        .headers(headers)
        .json(&request)
        .send()
        .await
        .map_err(|e| TryRequestError::Other(e.into()))?;

    if !resp.status().is_success() {
        let status = resp.status();

        // For 429 rate limit, pass through the response to allow proper error handling upstream
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(std::time::Duration::from_secs);

            // Record the rate limit hit for tracking
            record_rate_limit_hit(tier_name);
            ratelimit_tracker.record_429(tier_name, retry_after);
            record_rate_limit_backoff(tier_name);

            // Pass through the 429 response as-is
            let mut builder =
                axum::response::Response::builder().status(reqwest_status_to_axum(status));

            // Copy headers
            for (key, value) in resp.headers() {
                if let Ok(name) = axum::http::HeaderName::from_bytes(key.as_str().as_bytes()) {
                    if let Ok(val) = axum::http::HeaderValue::from_bytes(value.as_bytes()) {
                        builder = builder.header(name, val);
                    }
                }
            }

            // Insert x-ccr-tier header
            builder = builder.header("x-ccr-tier", tier_name);

            let body_bytes = resp
                .bytes()
                .await
                .map_err(|e| TryRequestError::Other(e.into()))?;

            // Normalize the error response body to include required fields like 'code': 'rate_limited'
            let normalized_body = if let Ok(mut error_json) =
                serde_json::from_slice::<serde_json::Value>(&body_bytes)
            {
                if let Some(error_obj) = error_json.get_mut("error").and_then(|e| e.as_object_mut())
                {
                    error_obj.insert(
                        "type".to_string(),
                        serde_json::Value::String("rate_limit_error".to_string()),
                    );
                    error_obj.insert(
                        "code".to_string(),
                        serde_json::Value::String("rate_limited".to_string()),
                    );
                    if let Some(retry_after_secs) = retry_after {
                        error_obj.insert(
                            "retry_after".to_string(),
                            serde_json::json!(retry_after_secs.as_secs()),
                        );
                    }
                }
                serde_json::to_vec(&error_json).unwrap_or(body_bytes.to_vec())
            } else {
                body_bytes.to_vec()
            };

            return builder.body(Body::from(normalized_body)).map_err(|e| {
                TryRequestError::Other(anyhow::anyhow!("Failed to build response: {}", e))
            });
        }

        let body = resp
            .text()
            .await
            .map_err(|e| TryRequestError::Other(e.into()))?;
        return Err(TryRequestError::Other(anyhow::anyhow!(
            "Provider returned {} from {}: {}",
            status,
            url,
            body
        )));
    }

    if request.stream.unwrap_or(false) {
        // Use streaming token tracking for Anthropic protocol
        let rate_limit_info = extract_rate_limit_headers(&resp);
        let ctx = StreamVerifyCtx {
            tier_name: tier_name.to_string(),
            local_estimate,
            ratelimit_tracker: Some(ratelimit_tracker.clone()),
            rate_limit_info: Some(rate_limit_info),
        };

        let mut response =
            stream_anthropic_response_with_tracking(resp, config.sse_buffer_size(), ctx, chain)
                .await;
        insert_ccr_tier_header(&mut response, tier_name);
        Ok(response)
    } else {
        let rate_limit_info = extract_rate_limit_headers(&resp);
        ratelimit_tracker.record_success(tier_name, rate_limit_info.0, rate_limit_info.1);

        let status = reqwest_status_to_axum(resp.status());
        let body = resp
            .bytes()
            .await
            .map_err(|e| TryRequestError::Other(e.into()))?;

        if let Ok(anthropic_resp) = serde_json::from_slice::<AnthropicResponse>(&body) {
            // Estimate tokens when provider returns zeros (common for some Anthropic-compatible APIs)
            let mut input_tokens = anthropic_resp.usage.input_tokens;
            let mut output_tokens = anthropic_resp.usage.output_tokens;

            // Use pre-request estimate for input if provider returned 0
            if input_tokens == 0 && local_estimate > 0 {
                input_tokens = local_estimate;
                warn!(
                    tier = tier_name,
                    "Provider returned 0 input_tokens, using pre-request estimate: {}",
                    input_tokens
                );
            }

            // Estimate output tokens from response content if provider returned 0
            if output_tokens == 0 {
                let content_len: usize = anthropic_resp
                    .content
                    .iter()
                    .map(|block| match block {
                        AnthropicContentBlock::Text { text } => text.len(),
                        AnthropicContentBlock::Thinking { thinking, .. } => thinking.len(),
                        AnthropicContentBlock::ToolUse { input, .. } => input.to_string().len(),
                    })
                    .sum();
                if content_len > 0 {
                    output_tokens = (content_len / 4).max(1) as u64;
                    warn!(
                        tier = tier_name,
                        "Provider returned 0 output_tokens, estimated: {} (from {} chars)",
                        output_tokens,
                        content_len
                    );
                }
            }

            record_usage(tier_name, input_tokens, output_tokens, 0, 0);
            verify_token_usage(tier_name, local_estimate, input_tokens);

            let final_resp = if chain.is_empty() {
                anthropic_resp
            } else {
                let resp_value = serde_json::to_value(&anthropic_resp)
                    .map_err(|e| TryRequestError::Other(e.into()))?;
                let transformed = chain
                    .apply_response(resp_value)
                    .map_err(TryRequestError::Other)?;
                serde_json::from_value::<AnthropicResponse>(transformed).unwrap_or(anthropic_resp)
            };

            let response_body =
                serde_json::to_vec(&final_resp).map_err(|e| TryRequestError::Other(e.into()))?;
            let mut response = (StatusCode::OK, response_body).into_response();
            insert_ccr_tier_header(&mut response, tier_name);
            return Ok(response);
        }

        let mut response = (status, body).into_response();
        insert_ccr_tier_header(&mut response, tier_name);
        Ok(response)
    }
}

// ============================================================================
// Streaming Response Translation
// ============================================================================

use bytes::Bytes;
use futures::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use crate::metrics::{increment_active_streams, record_stream_backpressure};

/// Stream response with OpenAI -> Anthropic translation.
pub async fn stream_response_translated(
    resp: reqwest::Response,
    buffer_size: usize,
    verify_ctx: Option<StreamVerifyCtx>,
    model_name: &str,
    chain: TransformerChain,
) -> Response {
    increment_active_streams(1);

    let _model_name = model_name.to_string();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(buffer_size);

    tokio::spawn(async move {
        let mut stream = resp.bytes_stream();
        let mut decoder = SseFrameDecoder::new();
        let mut is_first = true;
        let mut accumulated_content = String::new();
        let mut accumulated_reasoning = String::new();
        let mut _has_reasoning = false;
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;

        loop {
            tokio::select! {
                chunk = stream.next() => {
                    let Some(chunk) = chunk else {
                        break;
                    };
                    match chunk {
                        Ok(bytes) => {
                            for frame in decoder.push(&bytes) {
                                let json_str = frame.data.trim();
                                if json_str == "[DONE]" || json_str.is_empty() {
                                    continue;
                                }

                                // Try to parse as OpenAI stream chunk
                                if let Ok(chunk) =
                                    serde_json::from_str::<OpenAIStreamChunk>(json_str)
                                {
                                    // Accumulate usage info
                                    if let Some(ref usage) = chunk.usage {
                                        input_tokens = usage.prompt_tokens;
                                        output_tokens = usage.completion_tokens;
                                    }

                                    // Translate to Anthropic events
                                    let events =
                                        translate_stream_chunk_to_anthropic(&chunk, is_first);
                                    is_first = false;

                                    for event in events {
                                        let event_json =
                                            serde_json::to_string(&event).unwrap_or_default();
                                        let sse_data = format!(
                                            "event: {}\ndata: {}\n\n",
                                            event.event_type, event_json
                                        );

                                        if tx.capacity() == 0 {
                                            record_stream_backpressure();
                                        }
                                        if tx.send(Ok(Bytes::from(sse_data))).await.is_err() {
                                            break;
                                        }
                                    }

                                    // Accumulate content for usage estimation
                                    if let Some(choice) = chunk.choices.first() {
                                        if let Some(ref content) = choice.delta.content {
                                            accumulated_content.push_str(content);
                                        }
                                        if let Some(ref reasoning) = choice.delta.reasoning_content
                                        {
                                            accumulated_reasoning.push_str(reasoning);
                                            _has_reasoning = true;
                                        }
                                    }
                                } else {
                                    // Pass through frames that don't parse as OpenAI chunks.
                                    let sse_data = frame.to_sse_string();
                                    if tx.send(Ok(Bytes::from(sse_data))).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            let _ = tx
                                .send(Err(std::io::Error::other(e.to_string())))
                                .await;
                            break;
                        }
                    }
                }
                _ = tx.closed() => {
                    tracing::debug!("Client disconnected, aborting upstream");
                    break;
                }
            }
        }

        // Send final stop events
        let usage = if input_tokens > 0 || output_tokens > 0 {
            Some(AnthropicUsage {
                input_tokens,
                output_tokens,
            })
        } else {
            // Estimate from accumulated content if no usage reported
            let estimated_output = (accumulated_content.len() + accumulated_reasoning.len()) / 4;
            Some(AnthropicUsage {
                input_tokens,
                output_tokens: estimated_output as u64,
            })
        };

        // Apply response transformers to final accumulated content if chain is not empty
        // For streaming, we apply transforms to the final accumulated message structure
        if !chain.is_empty() {
            // Build a minimal Anthropic-like response for transformation
            let mut content_blocks = Vec::new();
            if !accumulated_reasoning.is_empty() {
                content_blocks.push(serde_json::json!({
                    "type": "thinking",
                    "thinking": accumulated_reasoning,
                    "signature": ""
                }));
            }
            if !accumulated_content.is_empty() {
                content_blocks.push(serde_json::json!({
                    "type": "text",
                    "text": accumulated_content
                }));
            }

            let resp_value = serde_json::json!({
                "content": content_blocks,
                "usage": {
                    "input_tokens": input_tokens,
                    "output_tokens": output_tokens
                }
            });

            if let Ok(transformed) = chain.apply_response(resp_value) {
                // Extract transformed values (for potential future use)
                trace!(transformed_response = %serde_json::to_string(&transformed).unwrap_or_default(),
                       "streaming response transformed");
            }
        }

        let stop_events = create_stream_stop_events(usage.clone());
        for event in stop_events {
            let event_json = serde_json::to_string(&event).unwrap_or_default();
            let sse_data = format!("event: {}\ndata: {}\n\n", event.event_type, event_json);
            let _ = tx.send(Ok(Bytes::from(sse_data))).await;
        }

        // Record usage and verify token drift if we have context
        if let Some(ctx) = &verify_ctx {
            if let Some(ref usage) = usage {
                record_usage(
                    &ctx.tier_name,
                    usage.input_tokens,
                    usage.output_tokens,
                    0,
                    0,
                );
                verify_token_usage(&ctx.tier_name, ctx.local_estimate, usage.input_tokens);
            }
            // Clear rate limit backoff and update rate limit state on successful stream completion
            if let Some(ref tracker) = ctx.ratelimit_tracker {
                if let Some((remaining, reset_at)) = &ctx.rate_limit_info {
                    tracker.record_success(&ctx.tier_name, *remaining, *reset_at);
                } else {
                    tracker.record_success(&ctx.tier_name, None, None);
                }
            }
        }

        increment_active_streams(-1);
    });

    let body = Body::from_stream(ReceiverStream::new(rx));

    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
        .header(axum::http::header::CACHE_CONTROL, "no-cache")
        .header(axum::http::header::CONNECTION, "keep-alive")
        .body(body)
        .unwrap()
}

/// Stream Anthropic protocol response with token tracking.
///
/// Parses Anthropic SSE events (`message_delta` with usage) to track tokens.
/// Used for Anthropic-protocol providers that may not return token counts.
pub async fn stream_anthropic_response_with_tracking(
    resp: reqwest::Response,
    buffer_size: usize,
    verify_ctx: StreamVerifyCtx,
    chain: TransformerChain,
) -> Response {
    increment_active_streams(1);

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(buffer_size);
    let tier_name = verify_ctx.tier_name.clone();
    let local_estimate = verify_ctx.local_estimate;

    tokio::spawn(async move {
        let mut stream = resp.bytes_stream();
        let mut decoder = SseFrameDecoder::new();
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;
        let mut accumulated_content_len: usize = 0;

        loop {
            tokio::select! {
                chunk = stream.next() => {
                    let Some(chunk) = chunk else {
                        break;
                    };
                    match chunk {
                        Ok(bytes) => {
                            for frame in decoder.push(&bytes) {
                                let json_str = frame.data.trim();
                                if json_str == "[DONE]" || json_str.is_empty() {
                                    continue;
                                }

                                // Parse Anthropic SSE events to extract usage
                                if let Ok(event) = serde_json::from_str::<serde_json::Value>(json_str) {
                                    // Extract usage from message_delta events
                                    if let Some(usage) = event.get("usage") {
                                        if let Some(input) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                                            if input > 0 {
                                                input_tokens = input;
                                            }
                                        }
                                        if let Some(output) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                                            if output > 0 {
                                                output_tokens = output;
                                            }
                                        }
                                    }

                                    // Also track content length for estimation fallback
                                    if let Some(delta) = event.get("delta") {
                                        if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                                            accumulated_content_len += text.len();
                                        }
                                    }
                                    if let Some(content_block) = event.get("content_block") {
                                        if let Some(text) = content_block.get("text").and_then(|v| v.as_str()) {
                                            accumulated_content_len += text.len();
                                        }
                                    }
                                }

                                // Apply response transformers if chain is not empty
                                let output_frame = if !chain.is_empty() {
                                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) {
                                        if let Ok(transformed) = chain.apply_response(value) {
                                            serde_json::to_string(&transformed).unwrap_or_else(|_| json_str.to_string())
                                        } else {
                                            json_str.to_string()
                                        }
                                    } else {
                                        json_str.to_string()
                                    }
                                } else {
                                    json_str.to_string()
                                };

                                // Reconstruct SSE frame
                                let sse_data = if let Some(event_type) = &frame.event {
                                    format!("event: {}\ndata: {}\n\n", event_type, output_frame)
                                } else {
                                    format!("data: {}\n\n", output_frame)
                                };

                                if tx.capacity() == 0 {
                                    record_stream_backpressure();
                                }
                                if tx.send(Ok(Bytes::from(sse_data))).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(std::io::Error::other(e.to_string()))).await;
                            break;
                        }
                    }
                }
                _ = tx.closed() => {
                    tracing::debug!("Client disconnected, aborting Anthropic upstream");
                    break;
                }
            }
        }

        // Estimate tokens if provider returned zeros (some Anthropic-compatible APIs don't report usage)
        let final_input_tokens = if input_tokens == 0 && local_estimate > 0 {
            warn!(
                tier = %tier_name,
                "Anthropic stream returned 0 input_tokens, using pre-request estimate: {}",
                local_estimate
            );
            local_estimate
        } else {
            input_tokens
        };

        let final_output_tokens = if output_tokens == 0 && accumulated_content_len > 0 {
            let estimate = (accumulated_content_len / 4).max(1) as u64;
            warn!(
                tier = %tier_name,
                "Anthropic stream returned 0 output_tokens, estimated: {} (from {} chars)",
                estimate,
                accumulated_content_len
            );
            estimate
        } else {
            output_tokens
        };

        // Record usage
        record_usage(&tier_name, final_input_tokens, final_output_tokens, 0, 0);
        verify_token_usage(&tier_name, local_estimate, final_input_tokens);

        // Update rate limit state
        if let Some(ref tracker) = verify_ctx.ratelimit_tracker {
            if let Some((remaining, reset_at)) = &verify_ctx.rate_limit_info {
                tracker.record_success(&tier_name, *remaining, *reset_at);
            } else {
                tracker.record_success(&tier_name, None, None);
            }
        }

        increment_active_streams(-1);
    });

    let body = Body::from_stream(ReceiverStream::new(rx));

    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
        .header(axum::http::header::CACHE_CONTROL, "no-cache")
        .header(axum::http::header::CONNECTION, "keep-alive")
        .body(body)
        .unwrap()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_string_content() {
        let content = serde_json::Value::String("Hello world".to_string());
        assert_eq!(
            normalize_message_content(&content),
            serde_json::Value::String("Hello world".to_string())
        );
    }

    #[test]
    fn test_normalize_array_content() {
        let content = serde_json::json!([
            {"type": "text", "text": "Hello "},
            {"type": "text", "text": "world"}
        ]);
        assert_eq!(
            normalize_message_content(&content),
            serde_json::Value::String("Hello world".to_string())
        );
    }

    #[test]
    fn test_translate_request_with_system() {
        let request = AnthropicRequest {
            model: "claude-3".to_string(),
            messages: vec![Message {
                role: "human".to_string(),
                content: serde_json::Value::String("Hello".to_string()),
                tool_call_id: None,
            }],
            system: Some(serde_json::Value::String("You are Claude.".to_string())),
            max_tokens: Some(1000),
            temperature: Some(0.7),
            stream: Some(false),
            tools: None,
        };

        let openai_req = translate_request_anthropic_to_openai(&request, "gpt-4");

        assert_eq!(openai_req.messages.len(), 2);
        assert_eq!(openai_req.messages[0].role, "system");
        assert_eq!(
            openai_req.messages[0].content,
            Some(serde_json::Value::String("You are Claude.".to_string()))
        );
        assert_eq!(openai_req.messages[1].role, "user");
        assert_eq!(
            openai_req.messages[1].content,
            Some(serde_json::Value::String("Hello".to_string()))
        );
    }

    #[test]
    fn test_translate_request_reasoning_model() {
        let request = AnthropicRequest {
            model: "deepseek-reasoner".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: serde_json::Value::String("Solve this.".to_string()),
                tool_call_id: None,
            }],
            system: None,
            max_tokens: Some(4000),
            temperature: None,
            stream: Some(true),
            tools: None,
        };

        let openai_req = translate_request_anthropic_to_openai(&request, "deepseek-reasoner");

        // Should use max_completion_tokens for reasoning models
        assert!(openai_req.max_tokens.is_none());
        assert_eq!(openai_req.max_completion_tokens, Some(4000));
        assert_eq!(openai_req.reasoning_effort, Some("high".to_string()));
    }

    #[test]
    fn test_translate_response_with_reasoning() {
        let openai_resp = OpenAIResponse {
            id: "resp_123".to_string(),
            object: "chat.completion".to_string(),
            created: 1234567890,
            model: "deepseek-reasoner".to_string(),
            choices: vec![OpenAIChoice {
                index: 0,
                message: OpenAIResponseMessage {
                    role: "assistant".to_string(),
                    content: Some(serde_json::Value::String("The answer is 42.".to_string())),
                    reasoning_content: Some("Let me think...".to_string()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: Some(OpenAIUsage {
                prompt_tokens: 10,
                completion_tokens: 20,
                prompt_tokens_details: None,
            }),
        };

        let anthropic_resp =
            translate_response_openai_to_anthropic(openai_resp, "deepseek-reasoner");

        assert_eq!(anthropic_resp.content.len(), 2);
        assert!(matches!(
            anthropic_resp.content[0],
            AnthropicContentBlock::Thinking { .. }
        ));
        assert!(matches!(
            anthropic_resp.content[1],
            AnthropicContentBlock::Text { .. }
        ));
        assert_eq!(anthropic_resp.usage.input_tokens, 10);
        assert_eq!(anthropic_resp.usage.output_tokens, 20);
    }

    #[test]
    fn test_translate_stream_chunk() {
        let chunk = OpenAIStreamChunk {
            id: "chunk_1".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 1234567890,
            model: "gpt-4".to_string(),
            choices: vec![OpenAIStreamChoice {
                index: 0,
                delta: OpenAIDelta {
                    role: Some("assistant".to_string()),
                    content: Some("Hello".to_string()),
                    reasoning_content: None,
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            usage: None,
        };

        let events = translate_stream_chunk_to_anthropic(&chunk, true);

        assert!(!events.is_empty());
        assert_eq!(events[0].event_type, "message_start");
        assert_eq!(events[1].event_type, "content_block_start");
    }

    #[test]
    fn test_translate_stream_chunk_with_reasoning() {
        let chunk = OpenAIStreamChunk {
            id: "chunk_1".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 1234567890,
            model: "deepseek-reasoner".to_string(),
            choices: vec![OpenAIStreamChoice {
                index: 0,
                delta: OpenAIDelta {
                    role: None,
                    content: None,
                    reasoning_content: Some("Analyzing...".to_string()),
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            usage: None,
        };

        let events = translate_stream_chunk_to_anthropic(&chunk, false);

        // Should have a thinking_delta event
        assert!(!events.is_empty());
        let delta = events[0].delta.as_ref().unwrap();
        assert_eq!(delta["type"], "thinking_delta");
    }

    #[test]
    fn test_translate_tool_result() {
        let request = AnthropicRequest {
            model: "claude-3".to_string(),
            messages: vec![
                Message {
                    role: "user".to_string(),
                    content: serde_json::json!("What's 2+2?"),
                    tool_call_id: None,
                },
                Message {
                    role: "assistant".to_string(),
                    content: serde_json::json!([{
                        "type": "tool_use",
                        "id": "toolu_123",
                        "name": "calculator",
                        "input": {"expression": "2+2"}
                    }]),
                    tool_call_id: None,
                },
                Message {
                    role: "user".to_string(),
                    content: serde_json::json!([{
                        "type": "tool_result",
                        "tool_use_id": "toolu_123",
                        "content": "4"
                    }]),
                    tool_call_id: None,
                },
            ],
            system: None,
            max_tokens: Some(1000),
            temperature: None,
            stream: None,
            tools: None,
        };

        let openai_req = translate_request_anthropic_to_openai(&request, "gpt-4");

        // Should have: user, assistant, tool messages
        let assistant_msg = openai_req
            .messages
            .iter()
            .find(|m| m.role == "assistant")
            .expect("Should have assistant message");
        let assistant_tool_calls = assistant_msg
            .tool_calls
            .as_ref()
            .expect("assistant should include tool_calls");
        assert_eq!(assistant_tool_calls.len(), 1);
        assert_eq!(assistant_tool_calls[0].id.as_deref(), Some("toolu_123"));
        assert_eq!(
            assistant_tool_calls[0]
                .function
                .as_ref()
                .map(|f| f.name.as_str()),
            Some("calculator")
        );

        let tool_msg = openai_req
            .messages
            .iter()
            .find(|m| m.role == "tool")
            .expect("Should have tool message");
        assert_eq!(
            tool_msg.content,
            Some(serde_json::Value::String("4".to_string()))
        );
        assert_eq!(tool_msg.tool_call_id, Some("toolu_123".to_string()));
    }

    #[test]
    fn test_translate_reasoning_model_adds_reasoning_content_for_assistant_tool_calls() {
        let request = AnthropicRequest {
            model: "deepseek-reasoner".to_string(),
            messages: vec![Message {
                role: "assistant".to_string(),
                content: serde_json::json!([{
                    "type": "tool_use",
                    "id": "call_abc",
                    "name": "calculator",
                    "input": {"expression": "2+2"}
                }]),
                tool_call_id: None,
            }],
            system: None,
            max_tokens: Some(1000),
            temperature: None,
            stream: Some(false),
            tools: None,
        };

        let openai_req = translate_request_anthropic_to_openai(&request, "deepseek-reasoner");
        assert_eq!(openai_req.messages.len(), 1);
        let msg = &openai_req.messages[0];
        assert_eq!(msg.role, "assistant");
        assert_eq!(msg.reasoning_content.as_deref(), Some(""));
        assert!(msg.tool_calls.is_some());
    }

    #[test]
    fn test_translate_preserves_tool_call_id_for_tool_role_message() {
        let request = AnthropicRequest {
            model: "deepseek-reasoner".to_string(),
            messages: vec![
                Message {
                    role: "assistant".to_string(),
                    content: serde_json::json!({
                        "tool_calls": [{
                            "id": "call_abc",
                            "type": "function",
                            "function": {
                                "name": "calculator",
                                "arguments": "{\"expression\":\"2+2\"}"
                            }
                        }]
                    }),
                    tool_call_id: None,
                },
                Message {
                    role: "tool".to_string(),
                    content: serde_json::Value::String("4".to_string()),
                    tool_call_id: Some("call_abc".to_string()),
                },
            ],
            system: None,
            max_tokens: Some(1000),
            temperature: None,
            stream: Some(false),
            tools: None,
        };

        let openai_req = translate_request_anthropic_to_openai(&request, "deepseek-reasoner");
        let tool_msg = openai_req
            .messages
            .iter()
            .find(|m| m.role == "tool")
            .expect("expected tool message");

        assert_eq!(tool_msg.tool_call_id.as_deref(), Some("call_abc"));
        assert_eq!(
            tool_msg.content,
            Some(serde_json::Value::String("4".to_string()))
        );
    }

    #[test]
    fn test_internal_request_to_anthropic_preserves_tool_call_id() {
        let internal = crate::frontend::InternalRequest {
            model: "deepseek,deepseek-reasoner".to_string(),
            messages: vec![
                crate::frontend::Message {
                    role: "user".to_string(),
                    content: serde_json::Value::String("use a tool".to_string()),
                    tool_call_id: None,
                },
                crate::frontend::Message {
                    role: "tool".to_string(),
                    content: serde_json::Value::String("4".to_string()),
                    tool_call_id: Some("call_abc".to_string()),
                },
            ],
            system: None,
            max_tokens: Some(512),
            temperature: None,
            stream: Some(false),
            tools: None,
            tool_choice: None,
            stop_sequences: None,
            extra_params: None,
        };

        let anthropic_req = internal_request_to_anthropic_request(internal);
        let tool_msg = anthropic_req
            .messages
            .iter()
            .find(|m| m.role == "tool")
            .expect("expected tool role message");
        assert_eq!(tool_msg.tool_call_id.as_deref(), Some("call_abc"));
    }

    #[test]
    fn test_responses_request_normalizes_developer_role() {
        let request = serde_json::json!({
            "model": "mock,test-model",
            "stream": false,
            "input": [{
                "type": "message",
                "role": "developer",
                "content": [{"type": "input_text", "text": "Follow policy"}]
            }]
        });

        let openai = responses_request_to_openai_chat_request(&request).unwrap();
        let messages = openai
            .get("messages")
            .and_then(|v| v.as_array())
            .expect("messages array");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "system");
    }
}
