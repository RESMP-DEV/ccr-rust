//! OpenAI to Anthropic format transformer.
//!
//! Converts OpenAI API format responses to Anthropic API format.
//! Handles:
//! - Response structure transformation (choices array → message)
//! - Content format transformation (string/objects → content blocks)
//! - Finish reason mapping (stop/length/tool_calls → end_turn/max_tokens/tool_use)
//! - Tool call format conversion

use crate::transformer::Transformer;
use anyhow::{anyhow, Result};
use serde_json::Value;
use tracing::debug;

/// OpenAI to Anthropic transformer.
///
/// Converts OpenAI API format responses to Anthropic API format.
/// This is the reverse of AnthropicToOpenaiTransformer.
///
/// Transformations:
/// - `choices[0].message` → `message` fields
/// - `finish_reason` mapping: stop→end_turn, length→max_tokens, tool_calls→tool_use
/// - `content` string → `[{"type": "text", "text": "..."}]`
/// - Tool calls → tool_use content blocks
#[derive(Debug, Clone)]
pub struct OpenaiToAnthropicTransformer;

impl Transformer for OpenaiToAnthropicTransformer {
    fn name(&self) -> &str {
        "openai-to-anthropic"
    }

    fn transform_response(&self, openai_response: Value) -> Result<Value> {
        // Extract the first choice (OpenAI can return multiple)
        let choice = openai_response
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .ok_or_else(|| anyhow!("OpenAI response missing 'choices' array or empty"))?;

        let message = choice
            .get("message")
            .ok_or_else(|| anyhow!("OpenAI choice missing 'message' field"))?;

        let id = openai_response
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("msg_unknown");

        let openai_finish_reason = choice
            .get("finish_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("stop");

        let stop_reason = map_openai_finish_reason(openai_finish_reason);

        // Transform content
        let content = transform_openai_content(message)?;

        // Handle tool calls if present
        let content = if let Some(tool_calls) = message.get("tool_calls").and_then(|t| t.as_array()) {
            let mut content_blocks = content.unwrap_or_default();
            for tool_call in tool_calls {
                if let Some(block) = convert_openai_tool_call(tool_call) {
                    content_blocks.push(block);
                }
            }
            content_blocks
        } else if let Some(c) = content {
            c
        } else {
            // Default content if none provided
            vec![serde_json::json!({
                "type": "text",
                "text": ""
            })]
        };

        // Build Anthropic format response
        let mut anthropic_response = serde_json::json!({
            "id": id,
            "type": "message",
            "role": "assistant",
            "content": content,
            "stop_reason": stop_reason,
        });

        // Copy usage if present
        if let Some(usage) = openai_response.get("usage") {
            anthropic_response["usage"] = usage.clone();
        }

        // Copy model if present
        if let Some(model) = openai_response.get("model") {
            anthropic_response["model"] = model.clone();
        }

        debug!(
            from = openai_finish_reason,
            to = stop_reason,
            "transformed OpenAI finish reason to Anthropic"
        );

        Ok(anthropic_response)
    }
}

/// Map OpenAI finish_reason to Anthropic stop_reason.
///
/// Mappings:
/// - `stop` → `end_turn`
/// - `length` → `max_tokens`
/// - `tool_calls` → `tool_use`
/// - `content_filter` → `stop_sequence` (fallback for safety filters)
/// - `error` → `end_turn` (treat as normal stop for error cases)
fn map_openai_finish_reason(openai_reason: &str) -> &str {
    match openai_reason {
        "stop" => "end_turn",
        "length" => "max_tokens",
        "tool_calls" => "tool_use",
        "content_filter" => "stop_sequence",
        "error" => "end_turn",
        _ => "end_turn", // Default fallback
    }
}

/// Transform OpenAI message content to Anthropic format.
///
/// OpenAI message content can be:
/// - A simple string: "Hello world"
/// - An array of content blocks (for multimodal): [{"type": "text", "text": "..."}, ...]
///
/// Anthropic always uses an array of content blocks.
fn transform_openai_content(message: &Value) -> Result<Option<Vec<Value>>> {
    match message.get("content") {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(vec![serde_json::json!({
            "type": "text",
            "text": text
        })])),
        Some(Value::Array(blocks)) => {
            let anthropic_blocks: Result<Vec<Value>, _> = blocks
                .iter()
                .map(|block| convert_openai_content_block(block))
                .collect();
            Ok(Some(anthropic_blocks?))
        }
        Some(other) => Err(anyhow!("Unexpected content type: {}", other)),
    }
}

/// Convert a single OpenAI content block to Anthropic format.
///
/// Handles:
/// - Text blocks: {"type": "text", "text": "..."}
/// - Image blocks: {"type": "image_url", "image_url": {"url": "..."}}
fn convert_openai_content_block(block: &Value) -> Result<Value> {
    let block_type = block
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Content block missing 'type'"))?;

    match block_type {
        "text" => Ok(serde_json::json!({
            "type": "text",
            "text": block.get("text").and_then(|v| v.as_str()).unwrap_or("")
        })),
        "image_url" => {
            let url = block
                .get("image_url")
                .and_then(|img| img.get("url"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("Image block missing 'url'"))?;

            // Parse data URL if present
            let (media_type, data) = if let Some(rest) = url.strip_prefix("data:") {
                let parts: Vec<&str> = rest.splitn(2, ';').collect();
                let media_type = parts.get(0).unwrap_or(&"image/jpeg");
                let data = parts
                    .get(1)
                    .and_then(|s| s.strip_prefix("base64,"))
                    .unwrap_or("");
                (*media_type, data)
            } else {
                // URL-based image
                return Ok(serde_json::json!({
                    "type": "image",
                    "source": {
                        "type": "url",
                        "url": url
                    }
                }));
            };

            Ok(serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": media_type,
                    "data": data
                }
            }))
        }
        _ => Err(anyhow!("Unsupported content block type: {}", block_type)),
    }
}

/// Convert an OpenAI tool call to an Anthropic tool_use content block.
///
/// OpenAI format:
/// ```json
/// {
///   "id": "call_abc123",
///   "type": "function",
///   "function": {
///     "name": "tool_name",
///     "arguments": "{\"key\": \"value\"}"
///   }
/// }
/// ```
///
/// Anthropic format:
/// ```json
/// {
///   "type": "tool_use",
///   "id": "toolu_abc123",
///   "name": "tool_name",
///   "input": {"key": "value"}
/// }
/// ```
fn convert_openai_tool_call(tool_call: &Value) -> Option<Value> {
    let function = tool_call.get("function")?;

    let name = function.get("name")?.as_str()?;
    let arguments_str = function.get("arguments")?.as_str()?;

    // Parse arguments JSON string into an object
    let input: Value = serde_json::from_str(arguments_str).ok()?;

    // Get or generate tool ID
    let id = tool_call
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            // Generate a simple ID if none provided
            // Using a static counter would be better but keeping it simple
            "toolu_unknown"
        });

    Some(serde_json::json!({
        "type": "tool_use",
        "id": id,
        "name": name,
        "input": input
    }))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_openai_finish_reason() {
        assert_eq!(map_openai_finish_reason("stop"), "end_turn");
        assert_eq!(map_openai_finish_reason("length"), "max_tokens");
        assert_eq!(map_openai_finish_reason("tool_calls"), "tool_use");
        assert_eq!(map_openai_finish_reason("content_filter"), "stop_sequence");
        assert_eq!(map_openai_finish_reason("error"), "end_turn");
        assert_eq!(map_openai_finish_reason("unknown"), "end_turn");
    }

    #[test]
    fn test_transform_openai_simple_response() {
        let transformer = OpenaiToAnthropicTransformer;

        let openai_response = serde_json::json!({
            "id": "chatcmpl-123",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Hello, world!"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5
            }
        });

        let result = transformer.transform_response(openai_response).unwrap();

        assert_eq!(result["id"], "chatcmpl-123");
        assert_eq!(result["type"], "message");
        assert_eq!(result["role"], "assistant");
        assert_eq!(result["stop_reason"], "end_turn");
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(result["content"][0]["text"], "Hello, world!");
        assert_eq!(result["usage"]["prompt_tokens"], 10);
    }

    #[test]
    fn test_transform_openai_max_tokens_response() {
        let transformer = OpenaiToAnthropicTransformer;

        let openai_response = serde_json::json!({
            "id": "chatcmpl-456",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "This is a long message that got cut off."
                },
                "finish_reason": "length"
            }]
        });

        let result = transformer.transform_response(openai_response).unwrap();
        assert_eq!(result["stop_reason"], "max_tokens");
    }

    #[test]
    fn test_transform_openai_tool_calls_response() {
        let transformer = OpenaiToAnthropicTransformer;

        let openai_response = serde_json::json!({
            "id": "chatcmpl-789",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "I'll call a tool for you.",
                    "tool_calls": [{
                        "id": "call_abc123",
                        "type": "function",
                        "function": {
                            "name": "calculator",
                            "arguments": "{\"operation\": \"add\", \"a\": 1, \"b\": 2}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let result = transformer.transform_response(openai_response).unwrap();

        assert_eq!(result["stop_reason"], "tool_use");
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(result["content"][0]["text"], "I'll call a tool for you.");

        assert_eq!(result["content"][1]["type"], "tool_use");
        assert_eq!(result["content"][1]["id"], "call_abc123");
        assert_eq!(result["content"][1]["name"], "calculator");
        assert_eq!(result["content"][1]["input"]["operation"], "add");
        assert_eq!(result["content"][1]["input"]["a"], 1);
        assert_eq!(result["content"][1]["input"]["b"], 2);
    }

    #[test]
    fn test_transform_openai_multimodal_response() {
        let transformer = OpenaiToAnthropicTransformer;

        let openai_response = serde_json::json!({
            "id": "chatcmpl-999",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "Here's an image:"},
                        {"type": "image_url", "image_url": {"url": "https://example.com/image.jpg"}}
                    ]
                },
                "finish_reason": "stop"
            }]
        });

        let result = transformer.transform_response(openai_response).unwrap();

        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(result["content"][0]["text"], "Here's an image:");
        assert_eq!(result["content"][1]["type"], "image");
        assert_eq!(result["content"][1]["source"]["type"], "url");
        assert_eq!(result["content"][1]["source"]["url"], "https://example.com/image.jpg");
    }

    #[test]
    fn test_transform_openai_base64_image_response() {
        let transformer = OpenaiToAnthropicTransformer;

        let openai_response = serde_json::json!({
            "id": "chatcmpl-888",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="}}
                    ]
                },
                "finish_reason": "stop"
            }]
        });

        let result = transformer.transform_response(openai_response).unwrap();

        assert_eq!(result["content"][0]["type"], "image");
        assert_eq!(result["content"][0]["source"]["type"], "base64");
        assert_eq!(result["content"][0]["source"]["media_type"], "image/png");
        assert!(result["content"][0]["source"]["data"].as_str().unwrap().starts_with("iVBORw"));
    }

    #[test]
    fn test_transform_openai_content_filter_response() {
        let transformer = OpenaiToAnthropicTransformer;

        let openai_response = serde_json::json!({
            "id": "chatcmpl-filtered",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "[Content filtered]"
                },
                "finish_reason": "content_filter"
            }]
        });

        let result = transformer.transform_response(openai_response).unwrap();
        assert_eq!(result["stop_reason"], "stop_sequence");
    }

    #[test]
    fn test_transform_openai_missing_choices() {
        let transformer = OpenaiToAnthropicTransformer;

        let openai_response = serde_json::json!({
            "id": "chatcmpl-bad",
            "choices": []
        });

        let result = transformer.transform_response(openai_response);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("choices"));
    }

    #[test]
    fn test_transformer_name() {
        let transformer = OpenaiToAnthropicTransformer;
        assert_eq!(transformer.name(), "openai-to-anthropic");
    }
}
