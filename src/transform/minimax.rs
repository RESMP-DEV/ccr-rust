// SPDX-License-Identifier: AGPL-3.0-or-later
//! MiniMax API transformer for modern MiniMax API.
//!
//! Handles MiniMax-specific request/response transformations:
//! - Request: model-specific handling (M3 vs M2.x)
//! - Request M3 (Anthropic): inject `thinking: {type: "adaptive"}`
//! - Request M2.x/M3 (OpenAI): inject `reasoning_split: true`
//! - Request: strip Anthropic-specific passthrough fields
//! - Response M2.x: map `reasoning_details` -> `reasoning_content`
//! - Response M3: preserve native `thinking` blocks, handle structured reasoning
//! - Response: convert thinking-only Anthropic responses to text content
//! - Response: pass cache token fields through unmodified (usage recording
//!   sums input and cache activity itself)
//!
//! Model capabilities:
//! - MiniMax-M3: 1M context, multimodal, native Anthropic-style thinking blocks
//! - MiniMax-M2.7, M2.5, M2.1: 204K context, reasoning_split format
//! - MiniMax-M2.7-highspeed, M2.5-highspeed: Faster versions of above

use crate::transformer::Transformer;
use anyhow::Result;
use serde_json::Value;
use tracing::{trace, warn};

pub(crate) const MINIMAX_M3_1_FLASH_PREVIEW: &str = "MiniMax-M3.1-Flash-Preview";
const MALFORMED_MINIMAX_PLACEHOLDER: &str = "[MALFORMED_MINIMAX_OUTPUT_REMOVED]";

/// Models that support native Anthropic-style thinking blocks (M3).
/// Entries are stored lowercase because `is_m3_model` lowercases its input first.
const M3_MODELS: &[&str] = &["minimax-m3"];

/// Models that use the reasoning_split format (M2.x)
const M2_MODELS: &[&str] = &[
    "MiniMax-M2.7",
    "MiniMax-M2.7-highspeed",
    "MiniMax-M2.5",
    "MiniMax-M2.5-highspeed",
    "MiniMax-M2.1",
    "MiniMax-M2.1-highspeed",
    "MiniMax-M2",
    "M2-her",
];

/// Check if a model is an M3 model (native Anthropic-style thinking)
pub(crate) fn is_m3_model(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    M3_MODELS.iter().any(|m| model == *m) || model.starts_with("minimax-m3.")
}

/// True when this provider talks to MiniMax, regardless of how the operator
/// named it. Multi-credential setups use names such as `minimax-anthropic`,
/// `minimax-primary`, or `minimax-work`, and `config.example.json` ships
/// `minimax-anthropic`, so an exact-name match would silently skip MiniMax
/// handling. Callers that resolve configuration or dispatch by provider must
/// share this predicate so they cannot disagree about which providers are
/// MiniMax.
pub(crate) fn is_minimax_provider_name(provider_name: &str, api_base_url: &str) -> bool {
    provider_name.to_ascii_lowercase().contains("minimax")
        || api_base_url.to_ascii_lowercase().contains("minimax")
}

/// Check if a model is an M2.x model (reasoning_split format)
fn is_m2_model(model: &str) -> bool {
    M2_MODELS.iter().any(|m| model.eq_ignore_ascii_case(m))
}

/// Detect MiniMax transport corruption in visible text.
///
/// The observed corruption is a *transport* signature, not ordinary prose: a
/// quote-only fragment, an embedded NUL, the provider delimiter
/// `]<]\u{200b}minimax[>[`, and tool tags that MiniMax leaks into visible text
/// with its zero-width marker (`<\u{200b}tool_call>`) or with injected
/// attributes (`<invoke name="write">`). Detection deliberately requires one of
/// those exact shapes so a legitimate answer that merely documents
/// `<invoke name="write">` or shows a plain `<tool_call>` example is not
/// discarded; an exact-string match alone cannot express that, so each pattern
/// below is checked for the zero-width marker or attribute form.
fn has_minimax_control_marker(text: &str) -> bool {
    // Provider delimiter and binary artifacts are unambiguous.
    if text.contains('\u{0}') || text.contains("]<]\u{200b}minimax[>[") {
        return true;
    }
    // Tool tags only count when they carry the zero-width transport marker.
    // MiniMax inserts U+200B directly after the opening angle bracket.
    if text.contains("<\u{200b}tool_call") || text.contains("<\u{200b}invoke") {
        return true;
    }
    false
}

fn is_malformed_minimax_text(text: &str) -> bool {
    let trimmed = text.trim();
    // Empty text is not itself corruption. Empty blocks are only removed
    // because they carry nothing; an empty assistant turn never reaches here
    // as a replacement candidate.
    let has_quote = trimmed
        .chars()
        .any(|ch| matches!(ch, '"' | '“' | '”' | '\'' | '‘' | '’'));
    let quote_only = has_quote
        && trimmed
            .chars()
            .all(|ch| ch.is_whitespace() || matches!(ch, '"' | '“' | '”' | '\'' | '‘' | '’'));
    quote_only || has_minimax_control_marker(trimmed)
}

fn strip_trailing_quote_artifact(text: &str) -> String {
    let core = text.trim_end_matches(|ch: char| {
        ch.is_whitespace() || matches!(ch, '"' | '“' | '”' | '\'' | '‘' | '’')
    });
    // `core` is produced by trimming a suffix off `text`, so it is always a
    // prefix of `text` and `core.len()` is a valid char boundary.
    let removed_quotes = text[core.len()..]
        .chars()
        .filter(|ch| matches!(ch, '"' | '“' | '”' | '\'' | '‘' | '’'))
        .count();
    if removed_quotes >= 2 {
        core.to_string()
    } else {
        text.to_string()
    }
}

fn has_text_block(blocks: &[Value]) -> bool {
    blocks
        .iter()
        .any(|block| block.get("type").and_then(Value::as_str) == Some("text"))
}

/// A surviving `tool_use` block already gives the turn a valid shape, so the
/// malformed-text placeholder is only needed when nothing usable remains.
fn needs_placeholder(blocks: &[Value], removed: bool) -> bool {
    removed
        && !has_text_block(blocks)
        && !blocks
            .iter()
            .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
}

fn sanitize_assistant_history(request: &mut Value) {
    let Some(messages) = request.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };

    for message in messages {
        if message
            .get("role")
            .and_then(Value::as_str)
            .is_some_and(|role| !role.eq_ignore_ascii_case("assistant"))
        {
            continue;
        }

        match message.get_mut("content") {
            Some(Value::Array(blocks)) => {
                let mut sanitized = Vec::with_capacity(blocks.len());
                let mut removed = false;
                for block in blocks.iter() {
                    let text = block.get("text").and_then(Value::as_str);
                    if block.get("type").and_then(Value::as_str) == Some("text") {
                        if let Some(text) = text {
                            if is_malformed_minimax_text(text) {
                                removed = true;
                                continue;
                            }
                            let clean = strip_trailing_quote_artifact(text);
                            if clean != text {
                                let mut clean_block = block.clone();
                                clean_block["text"] = Value::String(clean);
                                sanitized.push(clean_block);
                                continue;
                            }
                        }
                    }
                    sanitized.push(block.clone());
                }
                if needs_placeholder(&sanitized, removed) {
                    sanitized.insert(
                        0,
                        serde_json::json!({
                            "type": "text",
                            "text": MALFORMED_MINIMAX_PLACEHOLDER
                        }),
                    );
                }
                *blocks = sanitized;
            }
            Some(content @ Value::String(_)) => {
                let text = content.as_str().unwrap_or_default();
                if is_malformed_minimax_text(text) {
                    *content = Value::String(MALFORMED_MINIMAX_PLACEHOLDER.to_string());
                } else {
                    let clean = strip_trailing_quote_artifact(text);
                    if clean != text {
                        *content = Value::String(clean);
                    }
                }
            }
            _ => {}
        }
    }
}

fn sanitize_stream_delta(response: &mut Value) {
    if response.get("type").and_then(Value::as_str) != Some("content_block_delta") {
        return;
    }
    let Some(delta) = response.get_mut("delta") else {
        return;
    };
    if delta.get("type").and_then(Value::as_str) != Some("text_delta") {
        return;
    }
    let Some(text) = delta.get("text").and_then(Value::as_str) else {
        return;
    };
    // Individual deltas are fragments of a longer stream, so a whitespace-only
    // or quote-only delta is legitimate. Only strip confirmed transport
    // corruption here; a truncated artifact span such as `"]<]minimax[>[` still
    // contains its marker and is cleared, while ordinary spaces and quotation
    // marks survive untouched.
    let clean = if has_minimax_control_marker(text) {
        String::new()
    } else {
        text.to_string()
    };
    if clean != text {
        delta["text"] = Value::String(clean);
    }
}

fn sanitize_response_content(response: &mut Value) {
    let Some(content) = response.get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };
    let mut sanitized = Vec::with_capacity(content.len());
    let mut removed = false;
    for block in content.iter() {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                if is_malformed_minimax_text(text) {
                    removed = true;
                    continue;
                }
                let clean = strip_trailing_quote_artifact(text);
                if clean != text {
                    let mut clean_block = block.clone();
                    clean_block["text"] = Value::String(clean);
                    sanitized.push(clean_block);
                    continue;
                }
            }
        }
        sanitized.push(block.clone());
    }
    if needs_placeholder(&sanitized, removed) {
        sanitized.insert(
            0,
            serde_json::json!({
                "type": "text",
                "text": MALFORMED_MINIMAX_PLACEHOLDER
            }),
        );
    }
    *content = sanitized;
}

fn sanitize_response_text(response: &mut Value) {
    // Anthropic SSE applies the transformer to each frame, so text deltas need
    // the same artifact treatment as complete content arrays. Handle deltas
    // before checking for `content`, which delta frames intentionally do not
    // carry.
    sanitize_stream_delta(response);
    sanitize_response_content(response);
}

#[derive(Debug, Clone)]
pub struct MinimaxTransformer;

impl Transformer for MinimaxTransformer {
    fn name(&self) -> &str {
        "minimax"
    }

    fn transform_request(&self, mut request: Value) -> Result<Value> {
        let Some(obj) = request.as_object_mut() else {
            return Ok(request);
        };

        // Clone model name to avoid borrow issues during mutation
        let model = obj
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        if is_m3_model(&model) {
            obj.insert(
                "model".to_string(),
                Value::String(MINIMAX_M3_1_FLASH_PREVIEW.to_string()),
            );
        }

        if is_m3_model(&model) {
            // M3 uses native Anthropic-style thinking
            // For Anthropic format requests, inject thinking: {type: "adaptive"}
            if !obj.contains_key("thinking") {
                obj.insert(
                    "thinking".to_string(),
                    serde_json::json!({"type": "adaptive"}),
                );
                trace!("Injected thinking={{type=adaptive}} for M3 model {}", model);
            }
        } else if is_m2_model(&model) {
            // M2.x uses reasoning_split for OpenAI format
            // Enable reasoning_split for structured reasoning output
            obj.insert("reasoning_split".to_string(), Value::Bool(true));
            trace!("Injected reasoning_split=true for M2 model {}", model);
        } else {
            // Unknown model - apply both for compatibility
            if !obj.contains_key("thinking") {
                obj.insert(
                    "thinking".to_string(),
                    serde_json::json!({"type": "adaptive"}),
                );
            }
            obj.insert("reasoning_split".to_string(), Value::Bool(true));
            trace!(
                "Applied both thinking and reasoning_split for unknown model {}",
                model
            );
        }

        // Strip Anthropic-specific passthrough fields if present
        obj.remove("metadata");
        obj.remove("anthropic-beta");
        obj.remove("anthropic-version");
        obj.remove("anthropic_version");

        sanitize_assistant_history(&mut request);

        trace!("MiniMax request transformed for model {}", model);
        Ok(request)
    }

    fn transform_response(&self, mut response: Value) -> Result<Value> {
        sanitize_response_text(&mut response);

        // Handle Anthropic-format responses (from /anthropic/v1 endpoint)
        // If response has content array with only thinking blocks and no text,
        // convert the thinking to a text block to avoid empty responses
        if let Some(content) = response.get_mut("content") {
            if let Some(content_array) = content.as_array_mut() {
                let has_text = content_array
                    .iter()
                    .any(|block| block.get("type").and_then(|t| t.as_str()) == Some("text"));

                if !has_text {
                    // No text blocks - check for thinking blocks
                    let thinking_text: Vec<String> = content_array
                        .iter()
                        .filter_map(|block| {
                            if block.get("type").and_then(|t| t.as_str()) == Some("thinking") {
                                block
                                    .get("thinking")
                                    .and_then(|t| t.as_str())
                                    .map(|s| s.to_string())
                            } else {
                                None
                            }
                        })
                        .collect();

                    if !thinking_text.is_empty() {
                        warn!(
                            "MiniMax returned thinking-only response ({} blocks), converting to text",
                            thinking_text.len()
                        );
                        // Prepend a text block with the thinking content
                        let combined_thinking = thinking_text.join("\n\n");
                        content_array.insert(
                            0,
                            serde_json::json!({
                                "type": "text",
                                "text": format!("[Thinking]\n{}", combined_thinking)
                            }),
                        );
                    }
                }
            }
        }

        // Map reasoning_details -> reasoning_content in choices (OpenAI format for M2.x)
        if let Some(choices) = response.get_mut("choices") {
            if let Some(choices_array) = choices.as_array_mut() {
                for choice in choices_array {
                    // Handle message (non-streaming)
                    if let Some(message) = choice.get_mut("message") {
                        if let Some(obj) = message.as_object_mut() {
                            // M2.x returns reasoning_details
                            if let Some(reasoning) = obj.remove("reasoning_details") {
                                obj.insert("reasoning_content".to_string(), reasoning);
                            }
                            // M3 OpenAI format may return structured reasoning_content array
                            if let Some(reasoning_val) = obj.get("reasoning_content") {
                                if let Some(reasoning_arr) = reasoning_val.as_array() {
                                    // Extract text from structured reasoning array
                                    let reasoning_text: Vec<String> = reasoning_arr
                                        .iter()
                                        .filter_map(|item| {
                                            item.get("text")
                                                .and_then(|t| t.as_str())
                                                .map(|s| s.to_string())
                                        })
                                        .collect();
                                    if !reasoning_text.is_empty() {
                                        obj.insert(
                                            "reasoning_content".to_string(),
                                            Value::String(reasoning_text.join("\n\n")),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    // Handle delta (streaming)
                    if let Some(delta) = choice.get_mut("delta") {
                        if let Some(obj) = delta.as_object_mut() {
                            if let Some(reasoning) = obj.remove("reasoning_details") {
                                obj.insert("reasoning_content".to_string(), reasoning);
                            }
                            // Handle structured reasoning in streaming delta
                            if let Some(reasoning_val) = obj.get("reasoning_content") {
                                if let Some(reasoning_arr) = reasoning_val.as_array() {
                                    let reasoning_text: Vec<String> = reasoning_arr
                                        .iter()
                                        .filter_map(|item| {
                                            item.get("text")
                                                .and_then(|t| t.as_str())
                                                .map(|s| s.to_string())
                                        })
                                        .collect();
                                    if !reasoning_text.is_empty() {
                                        obj.insert(
                                            "reasoning_content".to_string(),
                                            Value::String(reasoning_text.join("\n\n")),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Usage passes through untouched: MiniMax reports cached tokens in
        // canonical Anthropic shape (`input_tokens` excludes cache activity),
        // and usage recording sums input plus cache reads and writes itself.
        // Folding the cache fields into `input_tokens` here would double-count
        // the cached share in /v1/usage and break the wire contract for
        // clients that sum the fields.
        trace!("MiniMax response transformed");
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_transform_request_m3_injects_thinking() {
        let transformer = MinimaxTransformer;
        let request = json!({
            "model": "MiniMax-M3",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 4096
        });

        let transformed = transformer.transform_request(request).unwrap();
        assert_eq!(transformed["thinking"]["type"], "adaptive");
        assert!(transformed.get("reasoning_split").is_none()); // M3 doesn't need reasoning_split
    }

    #[test]
    fn test_transform_request_m31_uses_native_thinking_and_canonical_model() {
        let transformer = MinimaxTransformer;
        let request = json!({
            "model": "MiniMax-M3.1-Flash",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 4096
        });

        let transformed = transformer.transform_request(request).unwrap();
        assert_eq!(transformed["model"], MINIMAX_M3_1_FLASH_PREVIEW);
        assert_eq!(transformed["thinking"]["type"], "adaptive");
        assert!(transformed.get("reasoning_split").is_none());
    }

    #[test]
    fn test_transform_request_sanitizes_malformed_assistant_history() {
        let transformer = MinimaxTransformer;
        let user_content = r#"Keep this literal <tool_call> example and quotes: ""."#;
        let request = json!({
            "model": MINIMAX_M3_1_FLASH_PREVIEW,
            "messages": [
                {"role": "user", "content": user_content},
                {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "\"\"<\u{200b}tool_call>\n<invoke name=\"write\">"},
                        {
                            "type": "tool_use",
                            "id": "toolu_123",
                            "name": "write",
                            "input": {"path": "test.txt"}
                        }
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        {"type": "tool_result", "tool_use_id": "toolu_123", "content": "ok"}
                    ]
                }
            ]
        });

        let transformed = transformer.transform_request(request).unwrap();
        let assistant_content = transformed["messages"][1]["content"].as_array().unwrap();

        assert_eq!(assistant_content.len(), 1);
        assert_eq!(assistant_content[0]["type"], "tool_use");
        assert_eq!(assistant_content[0]["id"], "toolu_123");
        assert_eq!(transformed["messages"][0]["content"], json!(user_content));
    }

    #[test]
    fn test_transform_request_replaces_malformed_string_assistant_history() {
        let transformer = MinimaxTransformer;
        let request = json!({
            "model": "MiniMax-M3",
            "messages": [
                {"role": "user", "content": "Use the tool."},
                {"role": "assistant", "content": "\"\""},
                {"role": "user", "content": "Try again."}
            ]
        });

        let transformed = transformer.transform_request(request).unwrap();

        assert_eq!(
            transformed["messages"][1]["content"],
            json!(MALFORMED_MINIMAX_PLACEHOLDER)
        );
        assert_eq!(
            transformed["messages"][0]["content"],
            json!("Use the tool.")
        );
    }

    #[test]
    fn test_valid_text_mentioning_markers_is_preserved() {
        let transformer = MinimaxTransformer;
        // An answer that legitimately discusses provider markup must survive.
        let answer = "The <invoke> tag is documented; see the <tool_call> example.";
        let response = json!({
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": answer}]
        });

        let transformed = transformer.transform_response(response).unwrap();
        let content = transformed["content"].as_array().unwrap();

        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["text"], json!(answer));
    }

    #[test]
    fn test_transport_marker_attributes_are_still_detected() {
        let transformer = MinimaxTransformer;
        let response = json!({
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "\"\"<\u{200b}invoke name=\"write\">"}]
        });

        let transformed = transformer.transform_response(response).unwrap();
        let content = transformed["content"].as_array().unwrap();

        assert_eq!(content[0]["text"], json!(MALFORMED_MINIMAX_PLACEHOLDER));
    }

    #[test]
    fn test_stream_delta_preserves_ordinary_whitespace_and_quotes() {
        let transformer = MinimaxTransformer;
        for fragment in [" ", "\"", "\"\"", "\n", "  \" "] {
            let frame = json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": fragment}
            });
            let transformed = transformer.transform_response(frame).unwrap();
            assert_eq!(
                transformed["delta"]["text"],
                json!(fragment),
                "delta {fragment:?} must pass through unchanged"
            );
        }
    }

    #[test]
    fn test_stream_delta_still_clears_transport_corruption() {
        let transformer = MinimaxTransformer;
        let frame = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "\"]<]\u{200b}minimax[>["}
        });

        let transformed = transformer.transform_response(frame).unwrap();
        assert_eq!(transformed["delta"]["text"], json!(""));
    }

    #[test]
    fn test_empty_text_block_is_not_replaced_with_placeholder() {
        let transformer = MinimaxTransformer;
        let request = json!({
            "model": "MiniMax-M3.1-Flash-Preview",
            "messages": [{"role": "assistant", "content": [{"type": "text", "text": ""}]}]
        });

        let transformed = transformer.transform_request(request).unwrap();
        let content = transformed["messages"][0]["content"].as_array().unwrap();

        assert_eq!(content[0]["text"], json!(""));
        assert_ne!(content[0]["text"], json!(MALFORMED_MINIMAX_PLACEHOLDER));
    }

    #[test]
    fn test_placeholder_inserted_when_only_thinking_block_remains() {
        let transformer = MinimaxTransformer;
        // Malformed text removed, thinking block kept: the placeholder must
        // still be present so thinking is never rendered as visible text.
        let response = json!({
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "\"\""},
                {"type": "thinking", "thinking": "internal", "signature": "sig"}
            ]
        });

        let transformed = transformer.transform_response(response).unwrap();
        let content = transformed["content"].as_array().unwrap();

        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], json!(MALFORMED_MINIMAX_PLACEHOLDER));
        assert!(content.iter().any(|b| b["type"] == json!("thinking")));
    }

    #[test]
    fn test_transform_request_m2_injects_reasoning_split() {
        let transformer = MinimaxTransformer;
        let request = json!({
            "model": "MiniMax-M2.7",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 4096
        });

        let transformed = transformer.transform_request(request).unwrap();
        assert_eq!(transformed["reasoning_split"], true);
        assert!(transformed.get("thinking").is_none()); // M2.x doesn't use thinking parameter
    }

    #[test]
    fn test_transform_request_strips_anthropic_fields() {
        let transformer = MinimaxTransformer;
        let request = json!({
            "model": "MiniMax-M3",
            "messages": [{"role": "user", "content": "Hello"}],
            "metadata": {"user_id": "abc"},
            "anthropic-beta": "tools-2024-04-04",
            "anthropic-version": "2023-06-01",
            "anthropic_version": "2023-06-01"
        });

        let transformed = transformer.transform_request(request).unwrap();
        assert_eq!(transformed["thinking"]["type"], "adaptive");
        assert!(transformed.get("metadata").is_none());
        assert!(transformed.get("anthropic-beta").is_none());
        assert!(transformed.get("anthropic-version").is_none());
        assert!(transformed.get("anthropic_version").is_none());
    }

    #[test]
    fn test_transform_request_preserves_existing_thinking() {
        let transformer = MinimaxTransformer;
        let request = json!({
            "model": "MiniMax-M3",
            "messages": [{"role": "user", "content": "Hello"}],
            "thinking": {"type": "enabled"}
        });

        let transformed = transformer.transform_request(request).unwrap();
        // Should preserve existing thinking parameter
        assert_eq!(transformed["thinking"]["type"], "enabled");
    }

    #[test]
    fn test_transform_response_maps_reasoning_details() {
        let transformer = MinimaxTransformer;
        let response = json!({
            "choices": [{
                "message": {
                    "reasoning_details": "Thinking..."
                }
            }]
        });

        let transformed = transformer.transform_response(response).unwrap();
        let message = &transformed["choices"][0]["message"];
        assert!(message.get("reasoning_details").is_none());
        assert_eq!(message["reasoning_content"], json!("Thinking..."));
    }

    #[test]
    fn test_transform_streaming_response_maps_reasoning_details() {
        let transformer = MinimaxTransformer;
        let response = json!({
            "choices": [{
                "delta": {
                    "reasoning_details": "Still thinking..."
                }
            }]
        });

        let transformed = transformer.transform_response(response).unwrap();
        let delta = &transformed["choices"][0]["delta"];
        assert!(delta.get("reasoning_details").is_none());
        assert_eq!(delta["reasoning_content"], json!("Still thinking..."));
    }

    #[test]
    fn test_transform_response_no_op_if_no_reasoning() {
        let transformer = MinimaxTransformer;
        let response = json!({
            "choices": [{
                "message": {
                    "content": "Hello there"
                }
            }]
        });
        let original_response = response.clone();

        let transformed = transformer.transform_response(response).unwrap();
        assert_eq!(transformed, original_response);
    }

    #[test]
    fn test_transform_anthropic_thinking_only_response() {
        let transformer = MinimaxTransformer;
        // MiniMax M3 Anthropic endpoint returning only thinking blocks
        let response = json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "thinking",
                "thinking": "The user wants me to say hello. I should respond warmly.",
                "signature": "abc123"
            }],
            "stop_reason": "max_tokens"
        });

        let transformed = transformer.transform_response(response).unwrap();
        let content = transformed["content"].as_array().unwrap();

        // Should have inserted a text block at the beginning
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert!(content[0]["text"].as_str().unwrap().contains("[Thinking]"));
        assert!(content[0]["text"]
            .as_str()
            .unwrap()
            .contains("The user wants me to say hello"));
        // Original thinking block should still be there
        assert_eq!(content[1]["type"], "thinking");
    }

    #[test]
    fn test_transform_anthropic_response_with_text_unchanged() {
        let transformer = MinimaxTransformer;
        // Response that already has text content should not be modified
        let response = json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "Let me think..."},
                {"type": "text", "text": "Hello!"}
            ],
            "stop_reason": "end_turn"
        });
        let original_content_len = response["content"].as_array().unwrap().len();

        let transformed = transformer.transform_response(response).unwrap();
        let content = transformed["content"].as_array().unwrap();

        // Should not insert additional text block
        assert_eq!(content.len(), original_content_len);
    }

    #[test]
    fn test_transform_response_removes_malformed_text_and_preserves_tool_use() {
        let transformer = MinimaxTransformer;
        let response = json!({
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "\"\"<\u{200b}tool_call>"},
                {
                    "type": "tool_use",
                    "id": "toolu_123",
                    "name": "write",
                    "input": {"path": "test.txt"}
                }
            ]
        });

        let transformed = transformer.transform_response(response).unwrap();
        let content = transformed["content"].as_array().unwrap();

        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "tool_use");
        assert_eq!(content[0]["id"], "toolu_123");
    }

    #[test]
    fn test_transform_response_replaces_text_only_malformed_output() {
        let transformer = MinimaxTransformer;
        let response = json!({
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "\"\""}  ]
        });

        let transformed = transformer.transform_response(response).unwrap();
        let content = transformed["content"].as_array().unwrap();

        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], json!(MALFORMED_MINIMAX_PLACEHOLDER));
    }

    #[test]
    fn test_transform_stream_delta_sanitizes_minimax_artifacts() {
        let transformer = MinimaxTransformer;
        let quote_only = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "\"\""}
        });
        let control_marker = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "<\u{200b}tool_call>"}
        });

        let transformed_quote = transformer.transform_response(quote_only).unwrap();
        let transformed_marker = transformer.transform_response(control_marker).unwrap();

        // A quote-only fragment is legitimate stream content mid-answer.
        assert_eq!(transformed_quote["delta"]["text"], json!("\"\""));
        // The transport marker is corruption and is cleared.
        assert_eq!(transformed_marker["delta"]["text"], json!(""));
    }

    #[test]
    fn test_trailing_quote_artifact_ignores_interleaved_whitespace() {
        assert_eq!(strip_trailing_quote_artifact("answer \" \""), "answer");
        assert_eq!(strip_trailing_quote_artifact("answer \""), "answer \"");
        assert_eq!(strip_trailing_quote_artifact("answer"), "answer");
    }

    #[test]
    fn test_transform_usage_preserves_cache_fields_unfolded() {
        let transformer = MinimaxTransformer;
        // MiniMax reports cache tokens separately in canonical Anthropic
        // shape. The transformer must not fold them into input_tokens:
        // usage recording sums the fields itself, and folding here would
        // double-count the cached share.
        let response = json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Hello"}],
            "usage": {
                "input_tokens": 1,
                "output_tokens": 242,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 40161
            }
        });

        let transformed = transformer.transform_response(response).unwrap();
        let usage = &transformed["usage"];

        assert_eq!(usage["input_tokens"], 1);
        assert_eq!(usage["output_tokens"], 242);
        assert_eq!(usage["cache_read_input_tokens"], 40161);
        assert_eq!(usage["cache_creation_input_tokens"], 0);
    }

    #[test]
    fn test_transform_usage_no_cache_unchanged() {
        let transformer = MinimaxTransformer;
        // Without cache tokens, input_tokens should stay the same
        let response = json!({
            "usage": {
                "input_tokens": 1000,
                "output_tokens": 500
            }
        });

        let transformed = transformer.transform_response(response).unwrap();
        assert_eq!(transformed["usage"]["input_tokens"], 1000);
    }

    #[test]
    fn test_transform_m3_structured_reasoning_content() {
        let transformer = MinimaxTransformer;
        // M3 OpenAI format returns structured reasoning_content array
        let response = json!({
            "choices": [{
                "message": {
                    "content": "Here is the answer.",
                    "reasoning_content": [
                        {
                            "type": "reasoning.text",
                            "id": "reasoning-text-1",
                            "text": "First, I need to understand the question."
                        },
                        {
                            "type": "reasoning.text",
                            "id": "reasoning-text-2",
                            "text": "Then, I'll solve it step by step."
                        }
                    ]
                }
            }]
        });

        let transformed = transformer.transform_response(response).unwrap();
        let message = &transformed["choices"][0]["message"];

        // Should extract and join reasoning text
        let reasoning = message["reasoning_content"].as_str().unwrap();
        assert!(reasoning.contains("First, I need to understand"));
        assert!(reasoning.contains("Then, I'll solve"));
    }

    #[test]
    fn test_is_m3_model() {
        assert!(is_m3_model("MiniMax-M3"));
        assert!(is_m3_model("minimax-m3"));
        assert!(is_m3_model("MINIMAX-M3"));
        // Every entry in M3_MODELS must be lowercase, because is_m3_model
        // lowercases its input before comparing.
        for entry in M3_MODELS {
            assert_eq!(*entry, entry.to_ascii_lowercase());
        }
        assert!(is_m3_model("MiniMax-M3.1-Flash"));
        assert!(is_m3_model("minimax-m3.1-flash-preview"));
        assert!(is_m3_model(" MiniMax-M3.1-Flash-Preview "));
        assert!(!is_m3_model("MiniMax-M2.7"));
        assert!(!is_m3_model("MiniMax-M2.5"));
    }

    #[test]
    fn test_is_m2_model() {
        assert!(is_m2_model("MiniMax-M2.7"));
        assert!(is_m2_model("MiniMax-M2.7-highspeed"));
        assert!(is_m2_model("MiniMax-M2.5"));
        assert!(is_m2_model("MiniMax-M2.1"));
        assert!(is_m2_model("MiniMax-M2"));
        assert!(!is_m2_model("MiniMax-M3"));
    }

    #[test]
    fn test_transform_request_unknown_model_applies_both() {
        let transformer = MinimaxTransformer;
        let request = json!({
            "model": "MiniMax-Unknown",
            "messages": [{"role": "user", "content": "Hello"}]
        });

        let transformed = transformer.transform_request(request).unwrap();
        // Should apply both for unknown models
        assert_eq!(transformed["thinking"]["type"], "adaptive");
        assert_eq!(transformed["reasoning_split"], true);
    }
}
