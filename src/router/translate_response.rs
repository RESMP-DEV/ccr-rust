// SPDX-License-Identifier: AGPL-3.0-or-later
use super::types::*;
use crate::transformer::{TransformerChain, TransformerRegistry};
use std::collections::BTreeMap;
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

    // Extract reasoning_content from the first choice for preservation
    let reasoning_content = openai_resp
        .choices
        .first()
        .and_then(|choice| choice.message.reasoning_content.clone());
    let refusal = openai_resp
        .choices
        .first()
        .and_then(|choice| choice.message.refusal.clone());
    let response_status = openai_resp.response_status.clone();
    let incomplete_details = openai_resp.incomplete_details.clone();

    let content = if let Some(choice) = openai_resp.choices.first() {
        let mut blocks: Vec<AnthropicContentBlock> = Vec::new();

        // Skip creating thinking blocks from reasoning_content for OpenAI-compatible
        // providers. Non-Anthropic models don't provide thinking signatures, and
        // emitting a thinking block with an empty signature causes
        // downstream Anthropic SDK clients to fail with
        // "reasoning part 0 not found".
        // Instead, reasoning_content is preserved in the response and serialized
        // back to OpenAI format.

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
        .map(|openai_usage| AnthropicUsage {
            input_tokens: openai_usage.prompt_tokens,
            output_tokens: openai_usage.completion_tokens,
            cache_read_input_tokens: openai_usage
                .prompt_tokens_details
                .as_ref()
                .and_then(|details| details.get("cached_tokens"))
                .and_then(serde_json::Value::as_u64),
            // OpenAI-style caching is automatic and reports no separate
            // cache-write count.
            cache_creation_input_tokens: None,
            reasoning_tokens: openai_usage
                .completion_tokens_details
                .as_ref()
                .and_then(|details| details.get("reasoning_tokens"))
                .and_then(serde_json::Value::as_u64),
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
        reasoning_content,
        refusal,
        response_status,
        incomplete_details,
    }
}

/// Tracks state across streaming chunks for proper Anthropic event sequencing.
///
/// OpenAI streams are stateless per-chunk, but Anthropic's protocol requires
/// matching `content_block_start`/`content_block_stop` pairs for every block.
/// This state tracks which blocks have been opened, which of them are already
/// closed, and the tool arguments accumulated so far, so every block can be
/// closed exactly once when the stream ends, whether it ended with a finish
/// reason, without one, on idle timeout, or on an upstream read error.
#[derive(Debug, Default)]
pub(super) struct StreamTranslationState {
    pub is_first: bool,
    pub text_block_started: bool,
    /// Anthropic-side indices of tool_use blocks we've started.
    pub active_tool_indices: Vec<usize>,
    /// The OpenAI finish_reason from the terminal chunk (e.g. "stop", "tool_calls").
    pub finish_reason: Option<String>,
    /// Anthropic-side block indices whose `content_block_stop` was emitted.
    pub closed_block_indices: Vec<usize>,
    /// Accumulated `input_json_delta.partial_json` per Anthropic tool block index.
    pub tool_arguments: BTreeMap<usize, String>,
}

impl StreamTranslationState {
    pub fn new() -> Self {
        Self {
            is_first: true,
            ..Default::default()
        }
    }
}

/// Build a `content_block_stop` event for one Anthropic block index.
fn content_block_stop_event(index: usize) -> AnthropicStreamEvent {
    AnthropicStreamEvent {
        event_type: "content_block_stop".to_string(),
        message: None,
        index: Some(index),
        content_block: None,
        delta: None,
        usage: None,
        stop_reason: None,
    }
}

/// Build an `input_json_delta` event carrying tool arguments.
fn input_json_delta_event(index: usize, partial_json: &str) -> AnthropicStreamEvent {
    AnthropicStreamEvent {
        event_type: "content_block_delta".to_string(),
        message: None,
        index: Some(index),
        content_block: None,
        delta: Some(serde_json::json!({
            "type": "input_json_delta",
            "partial_json": partial_json
        })),
        usage: None,
        stop_reason: None,
    }
}

/// Close every content block that is still open.
///
/// Called when a finish reason arrives and again at end of stream. Blocks that
/// are already closed are remembered, so each block is stopped exactly once
/// whether the stream ended with a finish reason, without one, on idle
/// timeout, or on an upstream read error. A tool block that never received
/// arguments gets a `{}` delta first, so its accumulated input stays valid
/// JSON for the client.
pub(super) fn close_open_content_blocks(
    state: &mut StreamTranslationState,
) -> Vec<AnthropicStreamEvent> {
    let mut events = Vec::new();

    if state.text_block_started && !state.closed_block_indices.contains(&0) {
        events.push(content_block_stop_event(0));
        state.closed_block_indices.push(0);
    }

    for index in state.active_tool_indices.clone() {
        if state.closed_block_indices.contains(&index) {
            continue;
        }
        let arguments = state
            .tool_arguments
            .get(&index)
            .map(String::as_str)
            .unwrap_or_default();
        if arguments.is_empty() {
            events.push(input_json_delta_event(index, "{}"));
        }
        events.push(content_block_stop_event(index));
        state.closed_block_indices.push(index);
    }

    events
}

/// Started tool blocks whose accumulated `partial_json` is not valid JSON.
///
/// A stream that loses its final frames leaves the tool arguments cut at
/// container depth 1, which the client reports as `EOF while parsing an
/// object`. Terminal-time validation surfaces the truncation instead of
/// closing the message as if it succeeded.
pub(super) fn invalid_tool_arguments(state: &StreamTranslationState) -> Vec<(usize, String)> {
    state
        .active_tool_indices
        .iter()
        .filter_map(|index| {
            let arguments = state.tool_arguments.get(index)?;
            if arguments.is_empty() || serde_json::from_str::<serde_json::Value>(arguments).is_ok()
            {
                return None;
            }
            Some((*index, arguments.clone()))
        })
        .collect()
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

                // `content_block_start` is emitted at most once per tool
                // index. A provider that repeats `id`/`name` on every chunk
                // must not make the client reset the block and lose the
                // accumulated `partial_json`.
                let has_tool_identity = tool_call.id.is_some()
                    || tool_call
                        .function
                        .as_ref()
                        .and_then(|f| f.name.as_ref())
                        .is_some();
                if has_tool_identity && !state.active_tool_indices.contains(&tool_index) {
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
                    state.active_tool_indices.push(tool_index);
                }

                if let Some(arguments) = tool_call
                    .function
                    .as_ref()
                    .and_then(|f| f.arguments.as_ref())
                    .filter(|a| !a.is_empty())
                {
                    state
                        .tool_arguments
                        .entry(tool_index)
                        .or_default()
                        .push_str(arguments);
                    events.push(input_json_delta_event(tool_index, arguments));
                }
            }
        }

        if let Some(ref reason) = choice.finish_reason {
            state.finish_reason = Some(reason.clone());
            events.extend(close_open_content_blocks(state));
        }
    }

    events
}

/// Create final Anthropic stream events (message_delta, message_stop).
///
/// `finish_reason` is the raw OpenAI finish reason (e.g. "stop", "tool_calls")
/// and is mapped to the Anthropic equivalent for the `stop_reason` field. A
/// missing or unknown finish reason yields a null `stop_reason`, so the
/// client can tell the message did not complete normally.
pub(super) fn create_stream_stop_events(
    usage: Option<AnthropicUsage>,
    finish_reason: Option<&str>,
) -> Vec<AnthropicStreamEvent> {
    let mut events = Vec::new();

    let usage = usage.unwrap_or_default();

    // A missing or unknown finish reason means the message did not complete
    // normally. Mapping it to `end_turn` would tell a conforming client the
    // message succeeded, so the terminal reports a null stop reason instead.
    let stop_reason = match finish_reason {
        Some("stop") | Some("function_call") => Some("end_turn"),
        Some("tool_calls") => Some("tool_use"),
        Some("length") => Some("max_tokens"),
        Some("content_filter") => Some("stop_sequence"),
        _ => None,
    };
    let stop_reason_value = match stop_reason {
        Some(reason) => serde_json::Value::String(reason.to_string()),
        None => serde_json::Value::Null,
    };

    events.push(AnthropicStreamEvent {
        event_type: "message_delta".to_string(),
        message: None,
        index: None,
        content_block: None,
        delta: Some(serde_json::json!({"stop_reason": stop_reason_value})),
        usage: Some(usage),
        stop_reason: None,
    });

    events.push(AnthropicStreamEvent {
        event_type: "message_stop".to_string(),
        message: None,
        index: None,
        content_block: None,
        delta: None,
        usage: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_call(
        index: usize,
        id: Option<&str>,
        name: Option<&str>,
        arguments: Option<&str>,
    ) -> OpenAIStreamToolCall {
        OpenAIStreamToolCall {
            index,
            id: id.map(str::to_string),
            function: Some(OpenAIStreamToolFunction {
                name: name.map(str::to_string),
                arguments: arguments.map(str::to_string),
            }),
        }
    }

    fn chunk(delta: OpenAIDelta, finish_reason: Option<&str>) -> OpenAIStreamChunk {
        OpenAIStreamChunk {
            id: "chatcmpl-test".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 1,
            model: "test-model".to_string(),
            choices: vec![OpenAIStreamChoice {
                index: 0,
                delta,
                finish_reason: finish_reason.map(str::to_string),
            }],
            usage: None,
        }
    }

    fn text_delta(text: &str) -> OpenAIDelta {
        OpenAIDelta {
            role: Some("assistant".to_string()),
            content: Some(text.to_string()),
            reasoning_content: None,
            tool_calls: None,
        }
    }

    fn tool_delta(calls: Vec<OpenAIStreamToolCall>) -> OpenAIDelta {
        OpenAIDelta {
            role: None,
            content: None,
            reasoning_content: None,
            tool_calls: Some(calls),
        }
    }

    #[test]
    fn open_blocks_close_at_end_of_stream_without_a_finish_reason() {
        let mut state = StreamTranslationState::new();
        let events = translate_stream_chunk_to_anthropic(
            &chunk(text_delta("I'll read the file."), None),
            &mut state,
        );
        assert!(events
            .iter()
            .any(|event| event.event_type == "content_block_start"));
        assert!(!events
            .iter()
            .any(|event| event.event_type == "content_block_stop"));

        let events = translate_stream_chunk_to_anthropic(
            &chunk(
                tool_delta(vec![tool_call(
                    0,
                    Some("call_1"),
                    Some("read_file"),
                    Some("{\"path\":\"a.txt\"}"),
                )]),
                None,
            ),
            &mut state,
        );
        assert!(!events
            .iter()
            .any(|event| event.event_type == "content_block_stop"));

        // End of stream closes the text block and the tool block exactly once.
        let stops = close_open_content_blocks(&mut state);
        assert_eq!(stops.len(), 2);
        assert_eq!(stops[0].event_type, "content_block_stop");
        assert_eq!(stops[0].index, Some(0));
        assert_eq!(stops[1].event_type, "content_block_stop");
        assert_eq!(stops[1].index, Some(1));
        assert!(close_open_content_blocks(&mut state).is_empty());
    }

    #[test]
    fn a_finish_reason_closes_every_block_once() {
        let mut state = StreamTranslationState::new();
        translate_stream_chunk_to_anthropic(&chunk(text_delta("hi"), None), &mut state);
        translate_stream_chunk_to_anthropic(
            &chunk(
                tool_delta(vec![tool_call(0, Some("call_1"), Some("read_file"), None)]),
                None,
            ),
            &mut state,
        );

        let events = translate_stream_chunk_to_anthropic(
            &chunk(OpenAIDelta::default(), Some("tool_calls")),
            &mut state,
        );
        let stops: Vec<usize> = events
            .iter()
            .filter(|event| event.event_type == "content_block_stop")
            .filter_map(|event| event.index)
            .collect();
        assert_eq!(stops, vec![0, 1]);
        // The tool block never received arguments, so it gets a `{}`
        // placeholder delta before its stop.
        assert!(events.iter().any(|event| {
            event.event_type == "content_block_delta"
                && event
                    .delta
                    .as_ref()
                    .is_some_and(|delta| delta["partial_json"] == "{}")
        }));
        // End of stream must not stop a block a second time.
        assert!(close_open_content_blocks(&mut state).is_empty());
    }

    #[test]
    fn a_repeated_tool_identity_does_not_start_the_block_twice() {
        let mut state = StreamTranslationState::new();
        let events = translate_stream_chunk_to_anthropic(
            &chunk(
                tool_delta(vec![tool_call(
                    0,
                    Some("call_dup"),
                    Some("read_file"),
                    None,
                )]),
                None,
            ),
            &mut state,
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == "content_block_start")
                .count(),
            2,
            "the text block and the tool block each start once"
        );

        let events = translate_stream_chunk_to_anthropic(
            &chunk(
                tool_delta(vec![tool_call(
                    0,
                    Some("call_dup"),
                    Some("read_file"),
                    Some("{\"path\":"),
                )]),
                None,
            ),
            &mut state,
        );
        assert!(
            !events
                .iter()
                .any(|event| event.event_type == "content_block_start"),
            "a repeated id/name must not emit a second content_block_start"
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "content_block_delta");
        assert_eq!(state.active_tool_indices, vec![1]);
    }

    #[test]
    fn a_missing_finish_reason_reports_a_null_stop_reason() {
        let events = create_stream_stop_events(None, None);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "message_delta");
        assert_eq!(
            events[0].delta.as_ref().unwrap()["stop_reason"],
            serde_json::Value::Null
        );
        assert_eq!(events[1].event_type, "message_stop");
    }

    #[test]
    fn known_finish_reasons_keep_their_stop_reason_mapping() {
        for (reason, expected) in [
            ("stop", "end_turn"),
            ("tool_calls", "tool_use"),
            ("length", "max_tokens"),
            ("content_filter", "stop_sequence"),
        ] {
            let events = create_stream_stop_events(None, Some(reason));
            assert_eq!(
                events[0].delta.as_ref().unwrap()["stop_reason"],
                serde_json::json!(expected),
                "finish reason {reason}"
            );
        }
    }

    #[test]
    fn an_unknown_finish_reason_is_not_reported_as_a_normal_end_of_turn() {
        let events = create_stream_stop_events(None, Some("mystery"));
        assert_eq!(
            events[0].delta.as_ref().unwrap()["stop_reason"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn truncated_tool_arguments_are_reported_at_terminal_time() {
        let mut state = StreamTranslationState::new();
        translate_stream_chunk_to_anthropic(
            &chunk(
                tool_delta(vec![
                    tool_call(
                        0,
                        Some("call_trunc"),
                        Some("write_file"),
                        Some("{\"content\":\"abc"),
                    ),
                    tool_call(
                        1,
                        Some("call_ok"),
                        Some("read_file"),
                        Some("{\"path\":\"a.txt\"}"),
                    ),
                    tool_call(2, Some("call_empty"), Some("list_files"), None),
                ]),
                None,
            ),
            &mut state,
        );

        let invalid = invalid_tool_arguments(&state);
        assert_eq!(invalid.len(), 1);
        assert_eq!(invalid[0].0, 1);
        assert_eq!(invalid[0].1, "{\"content\":\"abc");

        let events = close_open_content_blocks(&mut state);
        // text stop, truncated tool stop, valid tool stop, `{}` for the
        // argument-less tool, then its stop.
        assert_eq!(events.len(), 5);
        assert_eq!(events[0].event_type, "content_block_stop");
        assert_eq!(events[0].index, Some(0));
        assert_eq!(events[1].event_type, "content_block_stop");
        assert_eq!(events[1].index, Some(1));
        assert_eq!(events[2].event_type, "content_block_stop");
        assert_eq!(events[2].index, Some(2));
        assert_eq!(events[3].event_type, "content_block_delta");
        assert_eq!(events[3].index, Some(3));
        assert_eq!(events[3].delta.as_ref().unwrap()["partial_json"], "{}");
        assert_eq!(events[4].event_type, "content_block_stop");
        assert_eq!(events[4].index, Some(3));
    }
}
