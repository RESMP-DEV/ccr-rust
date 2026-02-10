//! Minimax M2.1 API transformer.
//!
//! Handles Minimax-specific request/response transformations:
//! - Request: add `reasoning_split: true` for interleaved thinking
//! - Request: strip Anthropic-specific passthrough fields if present
//! - Response: map `reasoning_details` -> `reasoning_content` (OpenAI format)
//! - Response: convert thinking-only Anthropic responses to text content
//!
//! M2.1 supports 204,800 token context window with up to 128K output tokens.
//! max_tokens is passed through unchanged - clients control their own limits.

use crate::transformer::Transformer;
use anyhow::Result;
use serde_json::Value;
use tracing::{trace, warn};

#[derive(Debug, Clone)]
pub struct MinimaxTransformer;

impl Transformer for MinimaxTransformer {
    fn name(&self) -> &str {
        "minimax"
    }

    fn transform_request(&self, mut request: Value) -> Result<Value> {
        if let Some(obj) = request.as_object_mut() {
            // Enable reasoning_split for structured reasoning output
            obj.insert("reasoning_split".to_string(), Value::Bool(true));

            // Strip Anthropic-specific passthrough fields if present.
            // These may appear if upstream clients forward Anthropic payloads/headers.
            obj.remove("metadata");
            obj.remove("anthropic-beta");
            obj.remove("anthropic-version");
            obj.remove("anthropic_version");
        }

        trace!("Minimax request transformed");
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
                            "Minimax returned thinking-only response ({} chars), converting to text",
                            thinking_text.iter().map(|s| s.len()).sum::<usize>()
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

        // Map reasoning_details -> reasoning_content in choices (OpenAI format)
        if let Some(choices) = response.get_mut("choices") {
            if let Some(choices_array) = choices.as_array_mut() {
                for choice in choices_array {
                    // Handle message (non-streaming)
                    if let Some(message) = choice.get_mut("message") {
                        if let Some(obj) = message.as_object_mut() {
                            if let Some(reasoning) = obj.remove("reasoning_details") {
                                obj.insert("reasoning_content".to_string(), reasoning);
                            }
                        }
                    }
                    // Handle delta (streaming)
                    if let Some(delta) = choice.get_mut("delta") {
                        if let Some(obj) = delta.as_object_mut() {
                            if let Some(reasoning) = obj.remove("reasoning_details") {
                                obj.insert("reasoning_content".to_string(), reasoning);
                            }
                        }
                    }
                }
            }
        }
        trace!("Minimax response transformed");
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_transform_request_adds_reasoning_split() {
        let transformer = MinimaxTransformer;
        let request = json!({
            "model": "minimax-m2.1",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 4096,
            "metadata": {"user_id": "abc"},
            "anthropic-beta": "tools-2024-04-04",
            "anthropic-version": "2023-06-01",
            "anthropic_version": "2023-06-01"
        });

        let transformed_request = transformer.transform_request(request).unwrap();
        assert_eq!(transformed_request["reasoning_split"], json!(true));
        // max_tokens passed through unchanged - M2.1 supports 128K output
        assert_eq!(transformed_request["max_tokens"], json!(4096));
        assert!(transformed_request.get("metadata").is_none());
        assert!(transformed_request.get("anthropic-beta").is_none());
        assert!(transformed_request.get("anthropic-version").is_none());
        assert!(transformed_request.get("anthropic_version").is_none());
    }

    #[test]
    fn test_transform_request_preserves_max_tokens() {
        let transformer = MinimaxTransformer;
        let request = json!({
            "model": "minimax-m2.1",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 100000
        });

        let transformed_request = transformer.transform_request(request).unwrap();
        // max_tokens passed through unchanged
        assert_eq!(transformed_request["max_tokens"], json!(100000));
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

        let transformed_response = transformer.transform_response(response).unwrap();
        let message = &transformed_response["choices"][0]["message"];
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

        let transformed_response = transformer.transform_response(response).unwrap();
        let delta = &transformed_response["choices"][0]["delta"];
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

        let transformed_response = transformer.transform_response(response).unwrap();
        assert_eq!(transformed_response, original_response);
    }

    #[test]
    fn test_transform_anthropic_thinking_only_response() {
        let transformer = MinimaxTransformer;
        // Simulates Minimax Anthropic endpoint returning only thinking blocks
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

        let transformed_response = transformer.transform_response(response).unwrap();
        let content = transformed_response["content"].as_array().unwrap();

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

        let transformed_response = transformer.transform_response(response).unwrap();
        let content = transformed_response["content"].as_array().unwrap();

        // Should not insert additional text block
        assert_eq!(content.len(), original_content_len);
    }
}
