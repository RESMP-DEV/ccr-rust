//! Anthropic to OpenAI format transformer.
//!
//! Converts Anthropic API format requests to OpenAI API format.
//! Handles:
//! - System prompt conversion (top-level field → system message)
//! - Message content blocks (array → string or keep array for multimodal)
//! - Tool definitions (input_schema → parameters)
//! - Tool choice (type conversion)
//! - Remove Anthropic-specific fields (metadata, stop_sequences)

use crate::transformer::Transformer;
use anyhow::Result;
use serde_json::Value;
use tracing::debug;

/// Anthropic to OpenAI transformer.
///
/// Converts Anthropic API format requests to OpenAI API format.
/// This is the reverse of OpenaiToAnthropicTransformer.
///
/// Transformations:
/// - `system` field → system message prepended to messages array
/// - Content blocks: array → string or keep as array for multimodal
/// - Tools: `input_schema` → `parameters`
/// - Tool choice: `{"type": "tool", "name": "..."}` → `{"type": "function", "function": {"name": "..."}}`
/// - Remove `metadata`, convert `stop_sequences` to `stop`
#[derive(Debug, Clone)]
pub struct AnthropicToOpenaiTransformer;

impl Transformer for AnthropicToOpenaiTransformer {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn transform_request(&self, mut request: Value) -> Result<Value> {
        // Extract and handle the system field (Anthropic-specific)
        // Anthropic: system field at top level
        // OpenAI: system message prepended to messages array
        if let Some(request_obj) = request.as_object_mut() {
            if let Some(system) = request_obj.remove("system") {
                // Convert system prompt to string (Anthropic allows string or array of blocks)
                let system_content = extract_system_content(system);

                // Prepend system message to messages array
                if let Some(messages) = request_obj.get_mut("messages") {
                    if let Some(messages_array) = messages.as_array_mut() {
                        let system_message = serde_json::json!({
                            "role": "system",
                            "content": system_content
                        });
                        messages_array.insert(0, system_message);
                    }
                } else {
                    // No messages array yet, create one with just the system message
                    let messages = serde_json::json!([{
                        "role": "system",
                        "content": system_content
                    }]);
                    request_obj.insert("messages".to_string(), messages);
                }
            }
        }

        // Transform messages from Anthropic content blocks to OpenAI format
        if let Some(messages) = request.get_mut("messages") {
            if let Some(messages_array) = messages.as_array_mut() {
                for message in messages_array {
                    if let Some(message_obj) = message.as_object_mut() {
                        if let Some(content) = message_obj.get_mut("content") {
                            // Anthropic: content can be a string or an array of content blocks
                            // OpenAI: content can be a string or an array (for multimodal)

                            // If content is a string, leave it as is (both formats support strings)
                            if content.is_string() {
                                continue;
                            }

                            // If content is an array, check if we need to convert it
                            if let Some(content_array) = content.as_array_mut() {
                                // Check if all blocks are text blocks with no special handling
                                let all_simple_text = content_array.iter().all(|block| {
                                    matches!(
                                        block.get("type").and_then(|t| t.as_str()),
                                        Some("text")
                                    )
                                });

                                if all_simple_text {
                                    // Convert single or multiple text blocks to a simple string
                                    let combined_text: String = content_array
                                        .iter()
                                        .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                                        .collect::<Vec<&str>>()
                                        .join("\n\n");
                                    *content = Value::String(combined_text);
                                } else {
                                    // Mixed content (text + image, etc.) - convert to OpenAI format
                                    // Anthropic: {"type": "image", "source": {"type": "base64", ...}}
                                    // OpenAI: {"type": "image_url", "image_url": {"url": "..."}}
                                    for block in content_array.iter_mut() {
                                        if let Some(block_obj) = block.as_object_mut() {
                                            if block_obj.get("type")
                                                == Some(&Value::String("image".to_string()))
                                            {
                                                if let Some(source) = block_obj.get("source") {
                                                    let image_url = if let Some(data) = source.get("data")
                                                    {
                                                        // Base64 encoded data
                                                        let media_type = source
                                                            .get("media_type")
                                                            .and_then(|m| m.as_str())
                                                            .unwrap_or("image/jpeg");
                                                        format!("data:{};base64,{}", media_type, data.as_str().unwrap_or(""))
                                                    } else {
                                                        // URL-based source
                                                        source
                                                            .get("url")
                                                            .and_then(|u| u.as_str())
                                                            .unwrap_or("")
                                                            .to_string()
                                                    };

                                                    *block = serde_json::json!({
                                                        "type": "image_url",
                                                        "image_url": {"url": image_url}
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Transform tools from Anthropic to OpenAI format
        if let Some(tools) = request.get_mut("tools") {
            if let Some(tools_array) = tools.as_array_mut() {
                for tool in tools_array {
                    if let Some(tool_obj) = tool.as_object_mut() {
                        // Anthropic: {"name": "...", "description": "...", "input_schema": {...}}
                        // OpenAI: {"type": "function", "function": {"name": "...", "description": "...", "parameters": {...}}}

                        let name = tool_obj.get("name").cloned();
                        let description = tool_obj.get("description").cloned();
                        let input_schema = tool_obj.get("input_schema").cloned();

                        if let (Some(n), Some(d), Some(is)) = (name, description, input_schema) {
                            *tool = serde_json::json!({
                                "type": "function",
                                "function": {
                                    "name": n,
                                    "description": d,
                                    "parameters": is
                                }
                            });
                        }
                    }
                }
            }
        }

        // Transform tool_choice from Anthropic to OpenAI format
        if let Some(tool_choice) = request.get_mut("tool_choice") {
            let new_tool_choice = match tool_choice {
                // Anthropic: {"type": "tool", "name": "..."}
                // OpenAI: {"type": "function", "function": {"name": "..."}}
                Value::Object(map)
                    if map.get("type") == Some(&Value::String("tool".to_string())) =>
                {
                    if let Some(name) = map.get("name").cloned() {
                        Some(serde_json::json!({
                            "type": "function",
                            "function": {"name": name}
                        }))
                    } else {
                        None
                    }
                }
                // Anthropic: "any" → OpenAI: "required"
                Value::String(s) if s == "any" => Some(Value::String("required".to_string())),
                // "auto" stays the same
                Value::String(s) if s == "auto" => None,
                // Already in OpenAI format, leave as is
                Value::Object(map)
                    if map.get("type") == Some(&Value::String("function".to_string())) =>
                {
                    None
                }
                _ => None,
            };

            if let Some(tc) = new_tool_choice {
                *tool_choice = tc;
            }
        }

        // max_tokens is required in Anthropic but optional in OpenAI - keep it as-is

        // Remove Anthropic-specific fields
        if let Some(request_obj) = request.as_object_mut() {
            // Remove metadata field (Anthropic-specific)
            request_obj.remove("metadata");

            // Convert stop_sequences to stop (if present)
            if let Some(stop_sequences) = request_obj.remove("stop_sequences") {
                // Anthropic: stop_sequences: ["string", ...]
                // OpenAI: stop: "string" or stop: ["string", ...]
                request_obj.insert("stop".to_string(), stop_sequences);
            }
        }

        Ok(request)
    }

    fn transform_response(&self, mut response: Value) -> Result<Value> {
        // Convert OpenAI tool calls back to Anthropic format
        if let Some(content) = response.get_mut("content") {
            if let Some(content_array) = content.as_array_mut() {
                for block in content_array {
                    if let Some(block_obj) = block.as_object_mut() {
                        // Ensure tool_use blocks have IDs
                        if block_obj.get("type")
                            == Some(&Value::String("tool_use".to_string()))
                        {
                            if !block_obj.contains_key("id") {
                                block_obj.insert(
                                    "id".to_string(),
                                    Value::String(format!("toolu_{}", generate_tool_id())),
                                );
                            }
                        }
                    }
                }
            }
        }
        Ok(response)
    }
}

/// Extract system content from Anthropic system field.
///
/// Anthropic allows system to be:
/// - A string: "You are a helpful assistant."
/// - An array of content blocks: [{"type": "text", "text": "..."}, {"type": "text", "text": "..."}]
///
/// Returns a string representation suitable for OpenAI's system message content.
fn extract_system_content(system: Value) -> String {
    if let Some(s) = system.as_str() {
        s.to_string()
    } else if let Some(blocks) = system.as_array() {
        // Handle array of content blocks
        let text_blocks: Vec<&str> = blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
            .collect();
        if text_blocks.is_empty() {
            // Fallback: try to serialize the array if no text blocks found
            system.to_string()
        } else {
            text_blocks.join("\n\n")
        }
    } else {
        // Unknown format, convert to string
        system.to_string()
    }
}

/// Generate a simple tool ID based on a counter.
///
/// Uses an atomic counter to avoid external dependencies.
fn generate_tool_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transformer_name() {
        let transformer = AnthropicToOpenaiTransformer;
        assert_eq!(transformer.name(), "anthropic");
    }

    #[test]
    fn test_extract_system_content_string() {
        let system = Value::String("You are a helpful assistant.".to_string());
        let result = extract_system_content(system);
        assert_eq!(result, "You are a helpful assistant.");
    }

    #[test]
    fn test_extract_system_content_array() {
        let system = serde_json::json!([
            {"type": "text", "text": "You are a helpful assistant."},
            {"type": "text", "text": "Be concise."}
        ]);
        let result = extract_system_content(system);
        assert_eq!(result, "You are a helpful assistant.\n\nBe concise.");
    }

    #[test]
    fn test_extract_system_content_empty_array() {
        let system = serde_json::json!([]);
        let result = extract_system_content(system);
        assert_eq!(result, "[]");
    }

    #[test]
    fn test_transform_request_system_string() {
        let transformer = AnthropicToOpenaiTransformer;

        let anthropic_request = serde_json::json!({
            "model": "claude-3",
            "system": "You are a helpful assistant.",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 1000
        });

        let result = transformer.transform_request(anthropic_request).unwrap();

        // System field should be removed
        assert!(result.get("system").is_none());

        // System message should be prepended
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are a helpful assistant.");
        assert_eq!(messages[1]["role"], "user");
    }

    #[test]
    fn test_transform_request_system_array() {
        let transformer = AnthropicToOpenaiTransformer;

        let anthropic_request = serde_json::json!({
            "model": "claude-3",
            "system": [
                {"type": "text", "text": "You are a helpful assistant."},
                {"type": "text", "text": "Be concise."}
            ],
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 1000
        });

        let result = transformer.transform_request(anthropic_request).unwrap();

        // System field should be removed
        assert!(result.get("system").is_none());

        // System message should be prepended with joined text
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(
            messages[0]["content"],
            "You are a helpful assistant.\n\nBe concise."
        );
    }

    #[test]
    fn test_transform_request_no_messages() {
        let transformer = AnthropicToOpenaiTransformer;

        let anthropic_request = serde_json::json!({
            "model": "claude-3",
            "system": "You are a helpful assistant.",
            "max_tokens": 1000
        });

        let result = transformer.transform_request(anthropic_request).unwrap();

        // Messages array should be created with just the system message
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "system");
    }

    #[test]
    fn test_transform_request_content_text_blocks() {
        let transformer = AnthropicToOpenaiTransformer;

        let anthropic_request = serde_json::json!({
            "model": "claude-3",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "Hello"},
                        {"type": "text", "text": "World"}
                    ]
                }
            ],
            "max_tokens": 1000
        });

        let result = transformer.transform_request(anthropic_request).unwrap();

        // Text blocks should be joined into a single string
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"], "Hello\n\nWorld");
    }

    #[test]
    fn test_transform_request_remove_metadata() {
        let transformer = AnthropicToOpenaiTransformer;

        let anthropic_request = serde_json::json!({
            "model": "claude-3",
            "metadata": {"user_id": "123"},
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 1000
        });

        let result = transformer.transform_request(anthropic_request).unwrap();

        // Metadata should be removed
        assert!(result.get("metadata").is_none());
    }

    #[test]
    fn test_transform_request_stop_sequences_to_stop() {
        let transformer = AnthropicToOpenaiTransformer;

        let anthropic_request = serde_json::json!({
            "model": "claude-3",
            "stop_sequences": ["STOP", "END"],
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 1000
        });

        let result = transformer.transform_request(anthropic_request).unwrap();

        // stop_sequences should be removed, stop should be added
        assert!(result.get("stop_sequences").is_none());
        assert_eq!(result["stop"], serde_json::json!(["STOP", "END"]));
    }

    #[test]
    fn test_transform_request_tool_choice_any_to_required() {
        let transformer = AnthropicToOpenaiTransformer;

        let anthropic_request = serde_json::json!({
            "model": "claude-3",
            "tool_choice": "any",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 1000
        });

        let result = transformer.transform_request(anthropic_request).unwrap();

        // "any" should become "required"
        assert_eq!(result["tool_choice"], "required");
    }

    #[test]
    fn test_transform_request_tool_choice_tool_to_function() {
        let transformer = AnthropicToOpenaiTransformer;

        let anthropic_request = serde_json::json!({
            "model": "claude-3",
            "tool_choice": {"type": "tool", "name": "calculator"},
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 1000
        });

        let result = transformer.transform_request(anthropic_request).unwrap();

        // {"type": "tool", "name": "..."} should become {"type": "function", "function": {"name": "..."}}
        assert_eq!(result["tool_choice"]["type"], "function");
        assert_eq!(result["tool_choice"]["function"]["name"], "calculator");
    }

    #[test]
    fn test_transform_request_tools_schema_to_parameters() {
        let transformer = AnthropicToOpenaiTransformer;

        let anthropic_request = serde_json::json!({
            "model": "claude-3",
            "tools": [
                {
                    "name": "calculator",
                    "description": "A calculator tool",
                    "input_schema": {
                        "type": "object",
                        "properties": {"a": {"type": "number"}}
                    }
                }
            ],
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 1000
        });

        let result = transformer.transform_request(anthropic_request).unwrap();

        // input_schema should become parameters under function object
        assert_eq!(result["tools"][0]["type"], "function");
        assert_eq!(result["tools"][0]["function"]["name"], "calculator");
        assert_eq!(result["tools"][0]["function"]["description"], "A calculator tool");
        assert_eq!(
            result["tools"][0]["function"]["parameters"]["type"],
            "object"
        );
    }

    #[test]
    fn test_transform_response_adds_tool_use_id() {
        let transformer = AnthropicToOpenaiTransformer;

        let mut response = serde_json::json!({
            "content": [
                {"type": "tool_use", "name": "calculator", "input": {"a": 1}}
            ]
        });

        let result = transformer.transform_response(response).unwrap();

        // Tool use should have an ID added
        let tool_use = result["content"][0].as_object().unwrap();
        assert!(tool_use.contains_key("id"));
        assert!(tool_use["id"].as_str().unwrap().starts_with("toolu_"));
    }

    #[test]
    fn test_transform_response_preserves_existing_tool_use_id() {
        let transformer = AnthropicToOpenaiTransformer;

        let mut response = serde_json::json!({
            "content": [
                {"type": "tool_use", "id": "existing_id", "name": "calculator", "input": {"a": 1}}
            ]
        });

        let result = transformer.transform_response(response).unwrap();

        // Existing ID should be preserved
        assert_eq!(result["content"][0]["id"], "existing_id");
    }
}
