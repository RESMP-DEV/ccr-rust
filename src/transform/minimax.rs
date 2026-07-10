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
//! - Response: normalize cache tokens in usage
//!
//! Model capabilities:
//! - MiniMax-M3: 1M context, multimodal, native Anthropic-style thinking blocks
//! - MiniMax-M2.7, M2.5, M2.1: 204K context, reasoning_split format
//! - MiniMax-M2.7-highspeed, M2.5-highspeed: Faster versions of above

use crate::transformer::Transformer;
use anyhow::Result;
use serde_json::Value;
use tracing::{trace, warn};

/// Models that support native Anthropic-style thinking blocks (M3)
const M3_MODELS: &[&str] = &["MiniMax-M3", "minimax-m3"];

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
fn is_m3_model(model: &str) -> bool {
    M3_MODELS
        .iter()
        .any(|m| model.eq_ignore_ascii_case(m) || model.eq_ignore_ascii_case(&m.to_lowercase()))
}

/// Check if a model is an M2.x model (reasoning_split format)
fn is_m2_model(model: &str) -> bool {
    M2_MODELS.iter().any(|m| model.eq_ignore_ascii_case(m))
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

        trace!("MiniMax request transformed for model {}", model);
        Ok(request)
    }

    fn transform_response(&self, mut response: Value) -> Result<Value> {
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

        // Normalize usage: MiniMax reports cached tokens separately
        // Total input = input_tokens + cache_creation_input_tokens + cache_read_input_tokens
        if let Some(usage) = response.get_mut("usage") {
            if let Some(obj) = usage.as_object_mut() {
                let input = obj
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let cache_creation = obj
                    .get("cache_creation_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let cache_read = obj
                    .get("cache_read_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                let total_input = input + cache_creation + cache_read;
                if total_input != input {
                    trace!(
                        "MiniMax usage normalized: {} + {} + {} = {} total input tokens",
                        input,
                        cache_creation,
                        cache_read,
                        total_input
                    );
                    obj.insert(
                        "input_tokens".to_string(),
                        Value::Number(total_input.into()),
                    );
                }
            }
        }

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
    fn test_transform_usage_normalizes_cache_tokens() {
        let transformer = MinimaxTransformer;
        // MiniMax reports cache tokens separately
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

        // Total input should be 1 + 0 + 40161 = 40162
        assert_eq!(usage["input_tokens"], 40162);
        assert_eq!(usage["output_tokens"], 242);
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
