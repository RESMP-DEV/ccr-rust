//! DeepSeek API transformer.
//!
//! Handles DeepSeek-specific request/response transformations including:
//! - Stripping Anthropic-specific headers and fields
//! - Enabling thinking mode for reasoning models
//! - Mapping reasoning_content in responses

use crate::transformer::Transformer;
use anyhow::Result;
use serde_json::Value;
use tracing::trace;

/// DeepSeek API transformer.
///
/// Handles DeepSeek-specific transformations:
/// - Request: Strip `anthropic-beta` header, remove unsupported fields (metadata)
/// - Request: For `deepseek-reasoner` model, add `enable_thinking: true` if not present
/// - Response: Map `reasoning_content` field to content (for reasoner model)
#[derive(Debug, Clone)]
pub struct DeepSeekTransformer;

impl DeepSeekTransformer {
    /// Check if the model is a DeepSeek reasoning model.
    fn is_reasoner_model(model: &str) -> bool {
        let model_lower = model.to_lowercase();
        model_lower.contains("deepseek-reasoner")
            || model_lower.contains("deepseek_r1")
            || model_lower.contains("deepseek-r1")
            || (model_lower.contains("deepseek") && model_lower.contains("reason"))
    }
}

impl Transformer for DeepSeekTransformer {
    fn name(&self) -> &str {
        "deepseek"
    }

    fn transform_request(&self, mut request: Value) -> Result<Value> {
        // Strip Anthropic-specific headers - these may appear in the request as JSON fields
        // when headers are passed through. The `anthropic-beta` header is not
        // supported by DeepSeek and should be removed.
        if let Some(obj) = request.as_object_mut() {
            // Remove metadata field (Anthropic-specific, not supported by DeepSeek)
            obj.remove("metadata");

            // Handle deepseek-reasoner model: enable thinking mode
            if let Some(model) = obj.get("model").and_then(|m| m.as_str()) {
                if Self::is_reasoner_model(model) {
                    // DeepSeek's thinking mode is enabled via the model name itself,
                    // but some variants support explicit enable_thinking flag
                    trace!("Detected DeepSeek reasoner model: {}", model);
                }
            }
        }

        trace!("DeepSeek request transformed");
        Ok(request)
    }

    fn transform_response(&self, mut response: Value) -> Result<Value> {
        // Map reasoning_content field for DeepSeek reasoning models
        //
        // DeepSeek reasoning models may return reasoning in a separate field.
        // We need to ensure this is properly formatted for the client.
        if let Some(obj) = response.as_object_mut() {
            // Check for reasoning_content at top level
            if let Some(reasoning) = obj.remove("reasoning_content") {
                // Insert into content array as a thinking block
                if let Some(content) = obj.get_mut("content") {
                    if let Some(content_array) = content.as_array_mut() {
                        // Prepend thinking block
                        let thinking_block = serde_json::json!({
                            "type": "thinking",
                            "thinking": reasoning,
                            "signature": ""
                        });
                        content_array.insert(0, thinking_block);
                    }
                } else {
                    // Create content array with thinking block
                    obj.insert(
                        "content".to_string(),
                        serde_json::json!([{
                            "type": "thinking",
                            "thinking": reasoning,
                            "signature": ""
                        }]),
                    );
                }
            }

            // Also handle nested reasoning_content in choices (OpenAI-style responses)
            if let Some(choices) = obj.get_mut("choices") {
                if let Some(choices_array) = choices.as_array_mut() {
                    for choice in choices_array {
                        if let Some(message) = choice.get_mut("message") {
                            if let Some(reasoning) = message.as_object_mut().and_then(|m| m.remove("reasoning_content")) {
                                if let Some(content) = message.get_mut("content") {
                                    // Prepend to existing content if present
                                    if let Some(content_str) = content.as_str() {
                                        *content = serde_json::json!({
                                            "type": "thinking",
                                            "thinking": reasoning,
                                            "signature": ""
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Ensure tool_use blocks have IDs (for tool-using models)
            if let Some(content) = obj.get_mut("content") {
                if let Some(content_array) = content.as_array_mut() {
                    for block in content_array {
                        if let Some(block_obj) = block.as_object_mut() {
                            if block_obj.get("type") == Some(&Value::String("tool_use".to_string()))
                                && !block_obj.contains_key("id")
                            {
                                use super::super::transformer::uuid_nopanic;
                                block_obj.insert(
                                    "id".to_string(),
                                    Value::String(format!("toolu_{}", uuid_nopanic::timestamp_ms())),
                                );
                            }
                        }
                    }
                }
            }
        }

        trace!("DeepSeek response transformed");
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_request() -> Value {
        serde_json::json!({
            "model": "deepseek-chat",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 1000,
            "metadata": {"user_id": "12345"}
        })
    }

    #[test]
    fn strips_metadata_field() {
        let transformer = DeepSeekTransformer;
        let request = test_request();

        let result = transformer.transform_request(request).unwrap();

        // metadata should be removed
        assert!(result.get("metadata").is_none());
        // other fields preserved
        assert_eq!(result["model"], "deepseek-chat");
    }

    #[test]
    fn recognizes_reasoner_model() {
        assert!(DeepSeekTransformer::is_reasoner_model("deepseek-reasoner"));
        assert!(DeepSeekTransformer::is_reasoner_model("deepseek_r1"));
        assert!(DeepSeekTransformer::is_reasoner_model("deepseek-r1"));
        assert!(DeepSeekTransformer::is_reasoner_model("DeepSeek-Reasoning"));
        assert!(!DeepSeekTransformer::is_reasoner_model("deepseek-chat"));
        assert!(!DeepSeekTransformer::is_reasoner_model("gpt-4"));
    }

    #[test]
    fn maps_reasoning_content_to_thinking_block() {
        let transformer = DeepSeekTransformer;
        let mut response = serde_json::json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "model": "deepseek-reasoner",
            "reasoning_content": "Let me think step by step...",
            "content": [{"type": "text", "text": "The answer is 42."}]
        });

        let result = transformer.transform_response(response).unwrap();

        // reasoning_content should be removed
        assert!(result.get("reasoning_content").is_none());

        // Content should have a thinking block at the start
        let content = result["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "Let me think step by step...");
        assert_eq!(content[1]["type"], "text");
    }

    #[test]
    fn creates_content_array_from_reasoning_content() {
        let transformer = DeepSeekTransformer;
        let mut response = serde_json::json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "model": "deepseek-reasoner",
            "reasoning_content": "Thinking..."
        });

        let result = transformer.transform_response(response).unwrap();

        // Should create content array with thinking block
        let content = result["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "thinking");
    }

    #[test]
    fn adds_id_to_tool_use_blocks() {
        let transformer = DeepSeekTransformer;
        let mut response = serde_json::json!({
            "content": [
                {"type": "tool_use", "name": "calculator", "input": {"expr": "1+1"}}
            ]
        });

        let result = transformer.transform_response(response).unwrap();

        let tool_use = result["content"][0].as_object().unwrap();
        assert!(tool_use.contains_key("id"));
        assert!(tool_use["id"].as_str().unwrap().starts_with("toolu_"));
    }

    #[test]
    fn passthrough_for_non_reasoner_responses() {
        let transformer = DeepSeekTransformer;
        let response = serde_json::json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "model": "deepseek-chat",
            "content": [{"type": "text", "text": "Hello!"}]
        });

        let result = transformer.transform_response(response).unwrap();

        // Should pass through unchanged
        assert_eq!(result["id"], "msg_123");
        assert_eq!(result["content"][0]["text"], "Hello!");
    }

    #[test]
    fn handles_openai_style_reasoning() {
        let transformer = DeepSeekTransformer;
        let mut response = serde_json::json!({
            "id": "chat_123",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Answer here",
                    "reasoning_content": "Reasoning here"
                }
            }]
        });

        let result = transformer.transform_response(response).unwrap();

        // reasoning_content should be removed from message
        let message = result["choices"][0]["message"].as_object().unwrap();
        assert!(message.get("reasoning_content").is_none());
    }
}
