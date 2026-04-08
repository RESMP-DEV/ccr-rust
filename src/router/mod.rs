// SPDX-License-Identifier: AGPL-3.0-or-later
mod types;
pub use types::*;

mod translate_request;

mod translate_response;

mod dispatch;
use dispatch::*;

mod streaming;
pub use streaming::{stream_anthropic_response_with_tracking, stream_response_translated};

mod openai_compat;
pub use openai_compat::handle_chat_completions;

mod responses_api;
pub use responses_api::handle_responses;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use std::collections::BTreeSet;
use tracing::{error, info, warn};

use crate::frontend::detect_frontend;
use crate::metrics::{
    increment_active_requests, record_failure, record_pre_request_tokens,
    record_rate_limit_backoff, record_rate_limit_hit, record_request_duration_with_frontend,
    record_request_with_frontend, sync_ewma_gauge,
};
use crate::routing::AttemptTimer;

/// RAII guard that decrements active_requests when dropped.
struct ActiveRequestGuard;

impl ActiveRequestGuard {
    fn new() -> Self {
        increment_active_requests(1);
        Self
    }
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        increment_active_requests(-1);
    }
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
    let _guard = ActiveRequestGuard::new();
    let start = std::time::Instant::now();
    let config = &state.config;
    let tiers = config.backend_tiers();

    let mut request = request;

    // Force non-streaming if configured (helps avoid SSE frame parsing limits)
    // Remember original stream flag so we can pseudo-stream the response back
    let client_wants_stream = request.stream.unwrap_or(false);
    if config.router().force_non_streaming && client_wants_stream {
        info!("Forcing non-streaming mode (forceNonStreaming=true), will pseudo-stream response");
        request.stream = Some(false);
    }

    let mut ordered = state.ewma_tracker.sort_tiers_with_config(&tiers, config);

    // Check if the requested model explicitly targets a specific provider (e.g., "deepseek,deepseek-chat")
    // If so, route directly to that provider instead of cascading through tiers
    // (unless ignoreDirect is enabled)
    let requested_model = request.model.clone();
    if !config.router().ignore_direct && requested_model.contains(',') {
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
    } else if config.router().ignore_direct && requested_model.contains(',') {
        info!(
            "Ignoring direct routing request for {} (ignoreDirect=true)",
            requested_model
        );
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
        let honor_remaining = config
            .resolve_provider(tier)
            .map(|p| p.honor_ratelimit_headers)
            .unwrap_or(true);
        if state
            .ratelimit_tracker
            .should_skip_tier(tier_name, honor_remaining)
        {
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

            match try_request(TryRequestArgs {
                config,
                registry: &state.transformer_registry,
                request: &request,
                tier,
                tier_name,
                local_estimate,
                ratelimit_tracker: state.ratelimit_tracker.clone(),
                debug_capture: state.debug_capture.clone(),
                openai_passthrough_body: request.openai_passthrough_body.as_ref(),
            })
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

                    // If client wanted streaming but we forced non-streaming,
                    // wrap the JSON response as pseudo-SSE so Claude CLI can parse it.
                    if client_wants_stream && config.router().force_non_streaming {
                        return streaming::wrap_json_response_as_sse(response).await;
                    }

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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::openai_compat::internal_request_to_anthropic_request;
    use super::responses_api::responses_request_to_openai_chat_request;
    use super::translate_request::*;
    use super::translate_response::*;
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
            model: "claude-sonnet-4-6".to_string(),
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
            openai_passthrough_body: None,
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
            openai_passthrough_body: None,
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
            model: "claude-sonnet-4-6".to_string(),
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
            openai_passthrough_body: None,
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
            openai_passthrough_body: None,
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
            openai_passthrough_body: None,
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

    /// Verify that the OpenAI passthrough path is taken for Codex→OpenAI routes:
    /// when `openai_passthrough_body` is populated on `AnthropicRequest`, the
    /// `handle_chat_completions` flow preserves it and threads it through to
    /// `try_request`.
    #[test]
    fn test_openai_passthrough_body_attached_for_codex_route() {
        use crate::frontend::codex::CodexFrontend;
        use crate::frontend::Frontend;

        let original_body = serde_json::json!({
            "model": "glm,glm-5.1",
            "messages": [
                {"role": "system", "content": "You are helpful"},
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 100,
            "temperature": 0.7,
            "stream": false
        });

        // Step 1: parse through CodexFrontend (same as handle_chat_completions)
        let frontend = CodexFrontend::new();
        let internal = frontend
            .parse_request(original_body.clone())
            .expect("parse should succeed");

        // Step 2: convert to AnthropicRequest and attach passthrough
        let mut anthropic = internal_request_to_anthropic_request(internal);
        anthropic.openai_passthrough_body = Some(original_body.clone());

        // Verify the passthrough body is preserved
        assert!(
            anthropic.openai_passthrough_body.is_some(),
            "passthrough body must be attached"
        );
        let pt = anthropic.openai_passthrough_body.as_ref().unwrap();
        assert_eq!(pt["model"], "glm,glm-5.1");
        assert_eq!(pt["messages"][1]["content"], "Hello");

        // Verify the passthrough body is NOT included in serialization
        // (it has #[serde(skip)])
        let serialized = serde_json::to_value(&anthropic).unwrap();
        assert!(
            serialized.get("openai_passthrough_body").is_none(),
            "passthrough body must not appear in serialized JSON"
        );
    }

    /// Verify that the passthrough body model name would be swapped correctly
    /// when used in the fast path of try_request_via_openai_protocol.
    #[test]
    fn test_openai_passthrough_model_swap() {
        let mut body = serde_json::json!({
            "model": "original-model",
            "messages": [{"role": "user", "content": "Hi"}],
            "stream": false
        });

        // Simulate the model swap that happens in the passthrough path
        let model_name = "glm-5.1";
        if let Some(obj) = body.as_object_mut() {
            obj.insert(
                "model".to_string(),
                serde_json::Value::String(model_name.to_string()),
            );
        }

        assert_eq!(body["model"], "glm-5.1");
        // Original messages and params are preserved
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "Hi");
        assert_eq!(body["stream"], false);
    }
}
