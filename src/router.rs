use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use tracing::{error, info, trace, warn};

use crate::config::Config;
use crate::metrics::{
    record_failure, record_pre_request_tokens, record_request, record_request_duration,
    record_usage, sync_ewma_gauge, verify_token_usage,
};
use crate::ratelimit::RateLimitTracker;
use crate::routing::{AttemptTimer, EwmaTracker};
use crate::sse::StreamVerifyCtx;
use crate::transformer::{TransformerChain, TransformerRegistry};

const MAX_RETRIES: usize = 3;

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
#[derive(Debug, Serialize)]
struct OpenAIRequest {
    model: String,
    messages: Vec<OpenAIMessage>,
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
#[derive(Debug, Serialize, Clone)]
struct OpenAIMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
}

/// OpenAI non-streaming response.
#[derive(Debug, Deserialize)]
struct OpenAIResponse {
    id: String,
    object: String,
    created: i64,
    model: String,
    choices: Vec<OpenAIChoice>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    index: u32,
    message: OpenAIResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct OpenAIResponseMessage {
    role: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default, rename = "reasoning_content")]
    reasoning_content: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct OpenAIUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: Option<serde_json::Value>,
}

/// OpenAI streaming response chunk.
#[derive(Debug, Deserialize)]
struct OpenAIStreamChunk {
    id: String,
    object: String,
    created: i64,
    model: String,
    choices: Vec<OpenAIStreamChoice>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamChoice {
    index: u32,
    delta: OpenAIDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct OpenAIDelta {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default, rename = "reasoning_content")]
    reasoning_content: Option<String>,
}

// ============================================================================
// Anthropic Response Types (Output)
// ============================================================================

/// Anthropic non-streaming response format.
#[derive(Debug, Serialize, Deserialize)]
struct AnthropicResponse {
    id: String,
    #[serde(rename = "type")]
    response_type: String,
    role: String,
    model: String,
    content: Vec<AnthropicContentBlock>,
    usage: AnthropicUsage,
    stop_reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String, signature: String },
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
struct AnthropicUsage {
    input_tokens: u64,
    output_tokens: u64,
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

/// Convert Anthropic message content to a plain string.
/// Handles both string content and array of content blocks.
fn normalize_message_content(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => {
            // Concatenate all text blocks from the array
            let mut result = String::new();
            for block in arr {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    result.push_str(text);
                } else if let Some(thinking) = block.get("thinking").and_then(|t| t.as_str()) {
                    // Include thinking content inline for OpenAI
                    if !result.is_empty() {
                        result.push_str("\n\n");
                    }
                    result.push_str("<thinking>");
                    result.push_str(thinking);
                    result.push_str("</thinking>");
                }
            }
            result
        }
        _ => content.as_str().unwrap_or("").to_string(),
    }
}

/// Translate Anthropic request format to OpenAI format.
fn translate_request_anthropic_to_openai(
    anthropic_req: &AnthropicRequest,
    model: &str,
) -> OpenAIRequest {
    let mut messages: Vec<OpenAIMessage> = Vec::new();

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
                content: Some(system_content),
                reasoning_content: None,
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

        let content = normalize_message_content(&msg.content);

        if !content.is_empty() || role != "user" {
            messages.push(OpenAIMessage {
                role: role.to_string(),
                content: Some(content),
                reasoning_content: None,
            });
        }
    }

    // Determine if this is a reasoning model (e.g., DeepSeek-R1)
    let is_reasoning_model = model.to_lowercase().contains("reasoner")
        || model.to_lowercase().contains("r1")
        || model.to_lowercase().contains("thinking");

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
        tools: anthropic_req.tools.clone(),
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
        if let Some(text) = &choice.message.content {
            if !text.is_empty() {
                blocks.push(AnthropicContentBlock::Text { text: text.clone() });
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
        model: model.to_string(),
        content,
        usage,
        stop_reason: openai_resp
            .choices
            .first()
            .and_then(|c| c.finish_reason.clone()),
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

pub async fn handle_messages(
    State(state): State<AppState>,
    Json(mut request): Json<AnthropicRequest>,
) -> Response {
    let start = std::time::Instant::now();
    let config = &state.config;
    let tiers = config.backend_tiers();

    info!("Incoming request for model: {}", request.model);

    // Sort tiers by observed EWMA latency (lowest first). Tiers without
    // enough samples keep their config-defined order.
    let ordered = state.ewma_tracker.sort_tiers(&tiers);

    // Serialize messages to JSON values once for pre-request token audit
    let msg_values: Vec<serde_json::Value> = request
        .messages
        .iter()
        .filter_map(|m| serde_json::to_value(m).ok())
        .collect();
    let tool_values: Option<Vec<serde_json::Value>> = request.tools.as_ref().map(|t| t.clone());

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

        for attempt in 0..max_retries {
            info!(
                "Trying {} ({}), attempt {}/{}",
                tier,
                tier_name,
                attempt + 1,
                max_retries
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
                &state.active_streams,
            )
            .await
            {
                Ok(response) => {
                    let attempt_duration = timer.finish_success();
                    let total_duration = start.elapsed().as_secs_f64();
                    record_request(tier_name);
                    record_request_duration(tier_name, total_duration);
                    sync_ewma_gauge(&state.ewma_tracker);
                    // Reset rate limit state on success
                    state.ratelimit_tracker.record_success(tier_name);
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
                    // Record 429 for backoff tracking
                    state.ratelimit_tracker.record_429(tier_name, retry_after);
                    // Skip remaining retries for this tier - move to next
                    break;
                }
                Err(TryRequestError::Other(e)) => {
                    timer.finish_failure();
                    sync_ewma_gauge(&state.ewma_tracker);
                    warn!("Failed {} attempt {}: {}", tier_name, attempt + 1, e);
                    record_failure(tier_name, "request_failed");

                    if attempt < max_retries - 1 {
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
    let total_attempts = ordered.len() * MAX_RETRIES;
    error!("All tiers exhausted after {} tier(s)", ordered.len());
    let error_resp = ErrorResponse {
        error: "All backend tiers failed".to_string(),
        tier: "all".to_string(),
        attempts: total_attempts,
    };

    (StatusCode::SERVICE_UNAVAILABLE, Json(error_resp)).into_response()
}

async fn try_request(
    config: &Config,
    registry: &TransformerRegistry,
    request: &AnthropicRequest,
    tier: &str,
    tier_name: &str,
    local_estimate: u64,
    active_streams: &Arc<AtomicUsize>,
) -> Result<Response, TryRequestError> {
    let provider = config.resolve_provider(tier).ok_or_else(|| {
        TryRequestError::Other(anyhow::anyhow!("Provider not found for tier: {}", tier))
    })?;

    // Build transformer chain from provider config
    let chain = build_transformer_chain(registry, provider, tier.split(',').nth(1).unwrap_or(tier));

    let url = format!(
        "{}/chat/completions",
        provider.api_base_url.trim_end_matches('/')
    );

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "Authorization",
        format!("Bearer {}", provider.api_key)
            .parse()
            .map_err(|e: reqwest::header::InvalidHeaderValue| TryRequestError::Other(anyhow::anyhow!("{}", e)))?,
    );
    headers.insert(
        "Content-Type",
        "application/json"
            .parse()
            .map_err(|e: reqwest::header::InvalidHeaderValue| TryRequestError::Other(anyhow::anyhow!("{}", e)))?,
    );

    // Extract the actual model name from the tier (format: "provider,model")
    let model_name = tier.split(',').nth(1).unwrap_or(tier);

    // Apply request transformers if chain is not empty
    let transformed_request = if chain.is_empty() {
        serde_json::to_value(request).map_err(|e| TryRequestError::Other(e.into()))?
    } else {
        let req_value = serde_json::to_value(request).map_err(|e| TryRequestError::Other(e.into()))?;
        chain.apply_request(req_value).map_err(|e| TryRequestError::Other(e))?
    };

    // Deserialize back to AnthropicRequest for translation
    let request: AnthropicRequest = serde_json::from_value(transformed_request).map_err(|e| TryRequestError::Other(e.into()))?;

    // Translate Anthropic request to OpenAI format
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

        // Check for 429 rate limit
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            // Parse Retry-After header if present
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(std::time::Duration::from_secs);
            return Err(TryRequestError::RateLimited(retry_after));
        }

        let body = resp
            .text()
            .await
            .map_err(|e| TryRequestError::Other(e.into()))?;
        return Err(TryRequestError::Other(anyhow::anyhow!(
            "Provider returned {}: {}",
            status,
            body
        )));
    }

    // Handle streaming vs non-streaming
    if request.stream.unwrap_or(false) {
        // For streaming, we need to translate the SSE events
        let ctx = StreamVerifyCtx {
            tier_name: tier_name.to_string(),
            local_estimate,
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
        // For non-streaming, translate the full response
        let body = resp
            .bytes()
            .await
            .map_err(|e| TryRequestError::Other(e.into()))?;

        // Try to parse as OpenAI response and translate
        if let Ok(openai_resp) = serde_json::from_slice::<OpenAIResponse>(&body) {
            // Record usage from the response
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

            // Translate to Anthropic format
            let anthropic_resp = translate_response_openai_to_anthropic(openai_resp, model_name);

            // Apply response transformers if chain is not empty
            let final_resp = if chain.is_empty() {
                anthropic_resp
            } else {
                let resp_value = serde_json::to_value(&anthropic_resp)
                    .map_err(|e| TryRequestError::Other(e.into()))?;
                let transformed = chain
                    .apply_response(resp_value)
                    .map_err(|e| TryRequestError::Other(e))?;
                serde_json::from_value::<AnthropicResponse>(transformed).unwrap_or(anthropic_resp)
            };

            let response_body =
                serde_json::to_vec(&final_resp).map_err(|e| TryRequestError::Other(e.into()))?;

            let mut response = (StatusCode::OK, response_body).into_response();
            response.headers_mut().insert(
                "x-ccr-tier",
                tier_name
                    .parse()
                    .unwrap_or(axum::http::HeaderValue::from_static("unknown")),
            );
            return Ok(response);
        }

        // Fallback: pass through original response if translation fails
        let mut response = (StatusCode::OK, body).into_response();
        response.headers_mut().insert(
            "x-ccr-tier",
            tier_name
                .parse()
                .unwrap_or(axum::http::HeaderValue::from_static("unknown")),
        );
        Ok(response)
    }
}

// ============================================================================
// Streaming Response Translation
// ============================================================================

use axum::body::Body;
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
                    // Parse SSE data lines
                    if let Ok(text) = std::str::from_utf8(&bytes) {
                        for line in text.lines() {
                            let data = line
                                .strip_prefix("data:")
                                .or_else(|| line.strip_prefix("data: "));
                            if let Some(json_str) = data {
                                let json_str = json_str.trim();
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
                                    // Pass through lines that don't parse as OpenAI chunks
                                    let sse_data = format!("{}\n", line);
                                    if tx.send(Ok(Bytes::from(sse_data))).await.is_err() {
                                        break;
                                    }
                                }
                            } else if !line.is_empty() {
                                // Pass through non-data lines
                                let sse_data = format!("{}\n", line);
                                if tx.send(Ok(Bytes::from(sse_data))).await.is_err() {
                                    break;
                                }
                            }
                        }
                    } else {
                        // Pass through binary data
                        if tx.capacity() == 0 {
                            record_stream_backpressure();
                        }
                        if tx.send(Ok(bytes)).await.is_err() {
                            break;
                        }
                    }
                }
                        Err(e) => {
                            let _ = tx
                                .send(Err(std::io::Error::new(
                                    std::io::ErrorKind::Other,
                                    e.to_string(),
                                )))
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
        assert_eq!(normalize_message_content(&content), "Hello world");
    }

    #[test]
    fn test_normalize_array_content() {
        let content = serde_json::json!([
            {"type": "text", "text": "Hello "},
            {"type": "text", "text": "world"}
        ]);
        assert_eq!(normalize_message_content(&content), "Hello world");
    }

    #[test]
    fn test_translate_request_with_system() {
        let request = AnthropicRequest {
            model: "claude-3".to_string(),
            messages: vec![Message {
                role: "human".to_string(),
                content: serde_json::Value::String("Hello".to_string()),
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
            Some("You are Claude.".to_string())
        );
        assert_eq!(openai_req.messages[1].role, "user");
        assert_eq!(openai_req.messages[1].content, Some("Hello".to_string()));
    }

    #[test]
    fn test_translate_request_reasoning_model() {
        let request = AnthropicRequest {
            model: "deepseek-reasoner".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: serde_json::Value::String("Solve this.".to_string()),
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
                    content: Some("The answer is 42.".to_string()),
                    reasoning_content: Some("Let me think...".to_string()),
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
}
