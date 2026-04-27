// SPDX-License-Identifier: AGPL-3.0-or-later
use super::types::*;
use crate::transformer::{TransformerChain, TransformerRegistry};
use tracing::debug;

// ============================================================================
// Response Translation: OpenAI -> Anthropic
// ============================================================================

/// Translate OpenAI non-streaming response to Anthropic format.
pub(super) fn translate_response_openai_to_anthropic(
    openai_resp: OpenAIResponse,
    model: &str,
) -> AnthropicResponse {
    debug!(model, response_id = %openai_resp.id, "translating OpenAI response to Anthropic format");
    let response_model = if openai_resp.model.is_empty() {
        model.to_string()
    } else {
        openai_resp.model.clone()
    };

    let content = if let Some(choice) = openai_resp.choices.first() {
        let mut blocks: Vec<AnthropicContentBlock> = Vec::new();

        // Skip reasoning content from OpenAI-compatible providers.
        // Non-Anthropic models don't provide thinking signatures, and
        // emitting a thinking block with an empty signature causes
        // downstream Anthropic SDK clients to fail with
        // "reasoning part 0 not found".
        // Reasoning is still available via the OpenAI response path.

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

/// Tracks state across streaming chunks for proper Anthropic event sequencing.
///
/// OpenAI streams are stateless per-chunk, but Anthropic's protocol requires
/// matching `content_block_start`/`content_block_stop` pairs for every block.
/// This state tracks which blocks have been opened so we can close them all
/// when `finish_reason` arrives.
#[derive(Debug, Default)]
pub(super) struct StreamTranslationState {
    pub is_first: bool,
    pub text_block_started: bool,
    /// Anthropic-side indices of tool_use blocks we've started.
    pub active_tool_indices: Vec<usize>,
    /// The OpenAI finish_reason from the terminal chunk (e.g. "stop", "tool_calls").
    pub finish_reason: Option<String>,
}

impl StreamTranslationState {
    pub fn new() -> Self {
        Self {
            is_first: true,
            ..Default::default()
        }
    }
}

/// Translate an OpenAI streaming chunk to Anthropic streaming events.
///
/// `state` is mutated to track which content blocks have been opened so that
/// `content_block_stop` events are emitted for every block when the stream ends.
pub(super) fn translate_stream_chunk_to_anthropic(
    chunk: &OpenAIStreamChunk,
    state: &mut StreamTranslationState,
) -> Vec<AnthropicStreamEvent> {
    let mut events = Vec::new();

    // Send message_start on first chunk
    if state.is_first {
        events.push(AnthropicStreamEvent {
            event_type: "message_start".to_string(),
            message: Some(serde_json::json!({
                "id": chunk.id,
                "type": "message",
                "role": "assistant",
                "model": chunk.model,
                "usage": {"input_tokens": 0, "output_tokens": 0}
            })),
            index: None,
            content_block: None,
            delta: None,
            usage: None,
            stop_reason: None,
        });

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
        state.text_block_started = true;
        state.is_first = false;
    }

    if let Some(choice) = chunk.choices.first() {
        // Skip reasoning content from OpenAI-compatible providers.
        // Non-Anthropic models lack thinking signatures, and emitting
        // thinking_delta events causes Anthropic SDK parse failures.

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
                    if !state.active_tool_indices.contains(&tool_index) {
                        state.active_tool_indices.push(tool_index);
                    }
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

        if let Some(ref reason) = choice.finish_reason {
            state.finish_reason = Some(reason.clone());

            if state.text_block_started {
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

            for &tool_idx in &state.active_tool_indices {
                events.push(AnthropicStreamEvent {
                    event_type: "content_block_stop".to_string(),
                    message: None,
                    index: Some(tool_idx),
                    content_block: None,
                    delta: None,
                    usage: None,
                    stop_reason: None,
                });
            }
        }
    }

    events
}

/// Create final Anthropic stream events (message_delta, message_stop).
///
/// `finish_reason` is the raw OpenAI finish reason (e.g. "stop", "tool_calls")
/// and is mapped to the Anthropic equivalent for the `stop_reason` field.
pub(super) fn create_stream_stop_events(
    usage: Option<AnthropicUsage>,
    finish_reason: Option<&str>,
) -> Vec<AnthropicStreamEvent> {
    let mut events = Vec::new();

    let usage = usage.unwrap_or_default();

    let stop_reason = match finish_reason {
        Some("tool_calls") => "tool_use",
        Some("length") => "max_tokens",
        Some("content_filter") => "stop_sequence",
        _ => "end_turn",
    };

    events.push(AnthropicStreamEvent {
        event_type: "message_delta".to_string(),
        message: None,
        index: None,
        content_block: None,
        delta: Some(serde_json::json!({"stop_reason": stop_reason})),
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
pub(super) fn build_transformer_chain(
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
