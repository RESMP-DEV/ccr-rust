//! OpenAI to Anthropic format transformer.
//!
//! Converts OpenAI API format requests and responses to Anthropic API format.
//! Handles:
//! - Request: /v1/chat/completions → /v1/messages format conversion
//! - System role message → top-level system field
//! - Messages array content blocks → Anthropic content format
//! - Tools (function format) → Anthropic input_schema format
//! - Response: choices array → message format
//! - Tool calls → tool_use content blocks

use crate::transformer::Transformer;
use anyhow::{anyhow, Result};
use serde_json::Value;
use tracing::debug;

/// OpenAI to Anthropic transformer.
///
/// Converts OpenAI API format requests and responses to Anthropic API format.
/// This is the reverse of AnthropicToOpenaiTransformer.
///
/// Request Transformations:
/// - System message(s) → top-level `system` field
/// - Content: string/array → Anthropic content blocks
/// - Tools: `function` wrapper → `input_schema` format
/// - Tool choice: OpenAI format → Anthropic format
/// - Remove OpenAI-specific fields (n, stop, etc.)
///
/// Response Transformations:
/// - `choices[0].message` → message fields
/// - `finish_reason` mapping: stop→end_turn, length→max_tokens, tool_calls→tool_use
/// - Tool calls → tool_use content blocks
#[derive(Debug, Clone)]
pub struct OpenAiToAnthropicTransformer;

/// Backwards-compatible alias for existing references.
pub type OpenaiToAnthropicTransformer = OpenAiToAnthropicTransformer;

impl Transformer for OpenAiToAnthropicTransformer {
    fn name(&self) -> &str {
        "openai-to-anthropic"
    }

    fn transform_request(&self, mut request: Value) -> Result<Value> {
        let request_obj = request
            .as_object_mut()
            .ok_or_else(|| anyhow!("Request must be a JSON object"))?;

        // Extract system messages and convert to top-level system field
        let system_content = extract_system_messages(request_obj)?;
        if !system_content.is_empty() {
            request_obj.insert("system".to_string(), Value::String(system_content));
        }

        // Transform messages from OpenAI format to Anthropic format
        if let Some(messages) = request_obj.get_mut("messages") {
            if let Some(messages_array) = messages.as_array_mut() {
                // Filter out system messages (already extracted) and transform content
                let mut transformed_messages = Vec::new();
                for message in messages_array.iter_mut() {
                    if let Some(message_obj) = message.as_object_mut() {
                        // Skip system messages - they're now in the top-level system field.
                        // Convert OpenAI tool result messages into Anthropic tool_result blocks.
                        if let Some(role) = message_obj.get("role").and_then(|r| r.as_str()) {
                            if role == "system" {
                                continue;
                            }
                            if role == "tool" {
                                transform_tool_result_message_to_anthropic(message_obj);
                                transformed_messages.push(message.clone());
                                continue;
                            }
                        }

                        // Transform message content
                        transform_message_content_to_anthropic(message_obj)?;
                        transformed_messages.push(message.clone());
                    }
                }
                *messages_array = transformed_messages;
            }
        }

        // Transform tools from OpenAI format to Anthropic format
        if let Some(tools) = request_obj.get_mut("tools") {
            if let Some(tools_array) = tools.as_array_mut() {
                for tool in tools_array {
                    if let Some(tool_obj) = tool.as_object_mut() {
                        // OpenAI: {"type": "function", "function": {"name": "...", "description": "...", "parameters": {...}}}
                        // Anthropic: {"name": "...", "description": "...", "input_schema": {...}}
                        if let Some(function) = tool_obj.remove("function") {
                            if let Some(func_obj) = function.as_object() {
                                let mut new_tool = serde_json::Map::new();

                                if let Some(name) = func_obj.get("name") {
                                    new_tool.insert("name".to_string(), name.clone());
                                }
                                if let Some(description) = func_obj.get("description") {
                                    new_tool.insert("description".to_string(), description.clone());
                                }
                                new_tool.insert(
                                    "input_schema".to_string(),
                                    func_obj
                                        .get("parameters")
                                        .cloned()
                                        .unwrap_or_else(default_input_schema),
                                );

                                // Replace the tool object contents with new tool
                                tool_obj.clear();
                                for (k, v) in new_tool {
                                    tool_obj.insert(k, v);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Transform tool_choice from OpenAI to Anthropic format
        let mut remove_tool_choice = false;
        if let Some(tool_choice) = request_obj.get_mut("tool_choice") {
            match tool_choice {
                // OpenAI: {"type": "function", "function": {"name": "..."}}
                // Anthropic: {"type": "tool", "name": "..."}
                Value::Object(map)
                    if map.get("type").and_then(|v| v.as_str()) == Some("function") =>
                {
                    if let Some(name) = map
                        .get("function")
                        .and_then(|f| f.as_object())
                        .and_then(|f| f.get("name"))
                        .cloned()
                    {
                        *tool_choice = serde_json::json!({
                            "type": "tool",
                            "name": name
                        });
                    } else {
                        remove_tool_choice = true;
                    }
                }
                // OpenAI: "required" → Anthropic: "any"
                Value::String(s) if s == "required" => {
                    *tool_choice = Value::String("any".to_string());
                }
                // OpenAI: "none" has no direct Anthropic equivalent. Drop it.
                Value::String(s) if s == "none" => {
                    remove_tool_choice = true;
                }
                // "auto" and Anthropic-style choices can pass through unchanged.
                _ => {}
            }
        }
        if remove_tool_choice {
            request_obj.remove("tool_choice");
        }

        // Remove OpenAI-specific fields that don't exist in Anthropic format
        request_obj.remove("n"); // Anthropic doesn't support multiple completions
        request_obj.remove("logprobs");
        request_obj.remove("top_logprobs");
        request_obj.remove("logit_bias");
        request_obj.remove("response_format");
        request_obj.remove("seed");

        // Convert 'stop' to 'stop_sequences'
        if let Some(stop) = request_obj.remove("stop") {
            request_obj.insert("stop_sequences".to_string(), stop);
        }

        // OpenAI may send either max_tokens or max_completion_tokens.
        // Convert max_completion_tokens to max_tokens if present.
        // If neither is present, let the API handle it - no artificial caps.
        if !request_obj.contains_key("max_tokens") {
            if let Some(max_completion_tokens) = request_obj.remove("max_completion_tokens") {
                request_obj.insert("max_tokens".to_string(), max_completion_tokens);
            }
        } else {
            // Prevent duplicate limits after conversion.
            request_obj.remove("max_completion_tokens");
        }

        debug!("transformed OpenAI request to Anthropic format");

        Ok(request)
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
        let content = if let Some(tool_calls) = message.get("tool_calls").and_then(|t| t.as_array())
        {
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

/// Extract system messages from the messages array and combine them.
///
/// OpenAI: System messages are in the messages array with role="system"
/// Anthropic: System prompt is a top-level string field
///
/// Returns combined system content as a string (empty if no system messages).
fn extract_system_messages(request_obj: &mut serde_json::Map<String, Value>) -> Result<String> {
    let mut system_parts = Vec::new();

    if let Some(messages) = request_obj.get_mut("messages") {
        if let Some(messages_array) = messages.as_array_mut() {
            for message in messages_array.iter() {
                if let Some(message_obj) = message.as_object() {
                    if let Some(role) = message_obj.get("role").and_then(|r| r.as_str()) {
                        if role == "system" {
                            // Extract content from system message
                            if let Some(content) = message_obj.get("content") {
                                let system_text = extract_text_content(content);
                                if !system_text.is_empty() {
                                    system_parts.push(system_text);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(system_parts.join("\n\n"))
}

/// Transform message content from OpenAI format to Anthropic format.
///
/// OpenAI: content can be a string or an array of content parts
/// Anthropic: content is always an array of content blocks
fn transform_message_content_to_anthropic(
    message_obj: &mut serde_json::Map<String, Value>,
) -> Result<()> {
    let tool_calls = message_obj.remove("tool_calls");

    if let Some(content) = message_obj.get_mut("content") {
        match content {
            Value::String(text) => {
                // Convert string to single text block
                *content = serde_json::json!([{
                    "type": "text",
                    "text": text
                }]);
            }
            Value::Null => {
                *content = Value::Array(Vec::new());
            }
            Value::Array(parts) => {
                // Already an array - convert OpenAI content parts to Anthropic blocks
                let mut blocks = Vec::new();
                for part in parts.iter() {
                    if let Some(part_obj) = part.as_object() {
                        if let Some(part_type) = part_obj.get("type").and_then(|t| t.as_str()) {
                            match part_type {
                                "text" => {
                                    blocks.push(serde_json::json!({
                                        "type": "text",
                                        "text": part_obj.get("text").and_then(|t| t.as_str()).unwrap_or("")
                                    }));
                                }
                                "image_url" => {
                                    // Convert OpenAI image_url to Anthropic image source
                                    if let Some(url) = part_obj
                                        .get("image_url")
                                        .and_then(|u| u.as_object())
                                        .and_then(|u| u.get("url"))
                                        .and_then(|u| u.as_str())
                                    {
                                        // Parse data URL if present
                                        if let Some(rest) = url.strip_prefix("data:") {
                                            let parts: Vec<&str> = rest.splitn(2, ';').collect();
                                            let media_type = parts.first().unwrap_or(&"image/jpeg");
                                            let data = parts
                                                .get(1)
                                                .and_then(|s| s.strip_prefix("base64,"))
                                                .unwrap_or("");
                                            blocks.push(serde_json::json!({
                                                "type": "image",
                                                "source": {
                                                    "type": "base64",
                                                    "media_type": media_type,
                                                    "data": data
                                                }
                                            }));
                                        } else {
                                            // URL-based image
                                            blocks.push(serde_json::json!({
                                                "type": "image",
                                                "source": {
                                                    "type": "url",
                                                    "url": url
                                                }
                                            }));
                                        }
                                    }
                                }
                                _ => {
                                    // Unknown type - include as-is
                                    blocks.push(part.clone());
                                }
                            }
                        }
                    } else if let Some(text) = part.as_str() {
                        blocks.push(serde_json::json!({
                            "type": "text",
                            "text": text
                        }));
                    }
                }
                if blocks.is_empty() {
                    blocks.push(serde_json::json!({
                        "type": "text",
                        "text": Value::Array(parts.clone()).to_string()
                    }));
                }
                *content = Value::Array(blocks);
            }
            _ => {
                // Other types - convert to string representation
                *content = serde_json::json!([{
                    "type": "text",
                    "text": content.to_string()
                }]);
            }
        }
    }

    // Convert assistant tool_calls payload into Anthropic tool_use content blocks.
    if let Some(tool_calls) = tool_calls {
        let tool_calls_array = match tool_calls {
            Value::Array(items) => items,
            _ => Vec::new(),
        };

        if !tool_calls_array.is_empty() {
            if !message_obj.contains_key("content") {
                message_obj.insert("content".to_string(), Value::Array(Vec::new()));
            }

            let content = message_obj
                .get_mut("content")
                .expect("content should exist after insertion");
            if content.is_null() {
                *content = Value::Array(Vec::new());
            } else if let Value::String(text) = content {
                *content = serde_json::json!([{
                    "type": "text",
                    "text": text.clone()
                }]);
            } else if !content.is_array() {
                let rendered = content.to_string();
                *content = serde_json::json!([{
                    "type": "text",
                    "text": rendered
                }]);
            }

            if let Some(content_array) = content.as_array_mut() {
                for tool_call in tool_calls_array {
                    if let Some(tool_use_block) = convert_openai_tool_call(&tool_call) {
                        content_array.push(tool_use_block);
                    }
                }
            }
        }
    }

    Ok(())
}

/// Convert an OpenAI `role: "tool"` message into Anthropic `tool_result` format.
fn transform_tool_result_message_to_anthropic(message_obj: &mut serde_json::Map<String, Value>) {
    let tool_use_id = message_obj
        .remove("tool_call_id")
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "toolu_unknown".to_string());

    let content = message_obj
        .remove("content")
        .map(|v| extract_text_content(&v))
        .unwrap_or_default();

    message_obj.insert("role".to_string(), Value::String("user".to_string()));
    message_obj.insert(
        "content".to_string(),
        serde_json::json!([{
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": content
        }]),
    );
}

/// Extract plain text from content that may be a string, block array, or object.
fn extract_text_content(content: &Value) -> String {
    match content {
        Value::String(text) => text.to_string(),
        Value::Array(parts) => {
            let mut texts = Vec::new();
            for part in parts {
                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                    texts.push(text.to_string());
                } else if let Some(text) = part.as_str() {
                    texts.push(text.to_string());
                }
            }
            if texts.is_empty() {
                content.to_string()
            } else {
                texts.join("\n\n")
            }
        }
        Value::Object(obj) => obj
            .get("text")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| content.to_string()),
        Value::Null => String::new(),
        _ => content.to_string(),
    }
}

fn default_input_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {}
    })
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
            let anthropic_blocks: Result<Vec<Value>, _> =
                blocks.iter().map(convert_openai_content_block).collect();
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
                let media_type = parts.first().unwrap_or(&"image/jpeg");
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
        .unwrap_or("toolu_unknown");

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
    fn test_transformer_name() {
        let transformer = OpenAiToAnthropicTransformer;
        assert_eq!(transformer.name(), "openai-to-anthropic");
    }

    #[test]
    fn test_transform_request_system_message_to_top_level() {
        let transformer = OpenAiToAnthropicTransformer;

        let openai_request = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 1000
        });

        let result = transformer.transform_request(openai_request).unwrap();

        // System field should be at top level
        assert_eq!(result["system"], "You are a helpful assistant.");

        // System message should be removed from messages array
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
    }

    #[test]
    fn test_transform_request_multiple_system_messages() {
        let transformer = OpenAiToAnthropicTransformer;

        let openai_request = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "system", "content": "Be concise."},
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 1000
        });

        let result = transformer.transform_request(openai_request).unwrap();

        // Multiple system messages should be joined
        assert_eq!(result["system"], "You are helpful.\n\nBe concise.");

        // Only user message should remain
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn test_transform_request_string_content_to_blocks() {
        let transformer = OpenAiToAnthropicTransformer;

        let openai_request = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 1000
        });

        let result = transformer.transform_request(openai_request).unwrap();

        // String content should be converted to content blocks
        let messages = result["messages"].as_array().unwrap();
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Hello");
    }

    #[test]
    fn test_transform_request_tools_conversion() {
        let transformer = OpenAiToAnthropicTransformer;

        let openai_request = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "calculator",
                        "description": "A calculator tool",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "a": {"type": "number"}
                            }
                        }
                    }
                }
            ],
            "max_tokens": 1000
        });

        let result = transformer.transform_request(openai_request).unwrap();

        // Tools should be converted from OpenAI format
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools[0]["name"], "calculator");
        assert_eq!(tools[0]["description"], "A calculator tool");
        assert_eq!(tools[0]["input_schema"]["type"], "object");
        assert!(tools[0].get("type").is_none()); // No type field in Anthropic format
        assert!(tools[0].get("function").is_none()); // No function wrapper
    }

    #[test]
    fn test_transform_request_tool_choice_required_to_any() {
        let transformer = OpenAiToAnthropicTransformer;

        let openai_request = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "tool_choice": "required",
            "max_tokens": 1000
        });

        let result = transformer.transform_request(openai_request).unwrap();

        // "required" should become "any"
        assert_eq!(result["tool_choice"], "any");
    }

    #[test]
    fn test_transform_request_tool_choice_function_to_tool() {
        let transformer = OpenAiToAnthropicTransformer;

        let openai_request = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "tool_choice": {
                "type": "function",
                "function": {"name": "calculator"}
            },
            "max_tokens": 1000
        });

        let result = transformer.transform_request(openai_request).unwrap();

        // {"type": "function", "function": {"name": "..."}} should become {"type": "tool", "name": "..."}
        assert_eq!(result["tool_choice"]["type"], "tool");
        assert_eq!(result["tool_choice"]["name"], "calculator");
    }

    #[test]
    fn test_transform_request_tool_choice_none_removed() {
        let transformer = OpenAiToAnthropicTransformer;

        let openai_request = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "tool_choice": "none",
            "max_tokens": 1000
        });

        let result = transformer.transform_request(openai_request).unwrap();
        assert!(result.get("tool_choice").is_none());
    }

    #[test]
    fn test_transform_request_stop_to_stop_sequences() {
        let transformer = OpenAiToAnthropicTransformer;

        let openai_request = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "stop": ["STOP", "END"],
            "max_tokens": 1000
        });

        let result = transformer.transform_request(openai_request).unwrap();

        // stop should become stop_sequences
        assert!(result.get("stop").is_none());
        assert_eq!(result["stop_sequences"], serde_json::json!(["STOP", "END"]));
    }

    #[test]
    fn test_transform_request_removes_openai_specific_fields() {
        let transformer = OpenAiToAnthropicTransformer;

        let openai_request = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "n": 3,
            "logprobs": true,
            "logit_bias": {"123": 100},
            "max_tokens": 1000
        });

        let result = transformer.transform_request(openai_request).unwrap();

        // OpenAI-specific fields should be removed
        assert!(result.get("n").is_none());
        assert!(result.get("logprobs").is_none());
        assert!(result.get("logit_bias").is_none());
    }

    #[test]
    fn test_transform_request_no_default_max_tokens() {
        let transformer = OpenAiToAnthropicTransformer;

        let openai_request = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}]
        });

        let result = transformer.transform_request(openai_request).unwrap();

        // No artificial max_tokens cap - let the API decide
        assert!(result.get("max_tokens").is_none());
    }

    #[test]
    fn test_transform_request_uses_max_completion_tokens() {
        let transformer = OpenAiToAnthropicTransformer;

        let openai_request = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_completion_tokens": 777
        });

        let result = transformer.transform_request(openai_request).unwrap();

        assert_eq!(result["max_tokens"], 777);
        assert!(result.get("max_completion_tokens").is_none());
    }

    #[test]
    fn test_transform_response_simple() {
        let transformer = OpenAiToAnthropicTransformer;

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
    fn test_transform_response_with_tool_calls() {
        let transformer = OpenAiToAnthropicTransformer;

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
        assert_eq!(result["content"][1]["type"], "tool_use");
        assert_eq!(result["content"][1]["id"], "call_abc123");
        assert_eq!(result["content"][1]["name"], "calculator");
        assert_eq!(result["content"][1]["input"]["operation"], "add");
    }

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
    fn test_extract_system_messages() {
        let mut request = serde_json::json!({
            "messages": [
                {"role": "system", "content": "First system"},
                {"role": "user", "content": "Hello"},
                {"role": "system", "content": "Second system"}
            ]
        });

        let system = extract_system_messages(request.as_object_mut().unwrap()).unwrap();
        assert_eq!(system, "First system\n\nSecond system");
    }

    #[test]
    fn test_transform_message_content_with_image() {
        let mut message = serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "What's in this image?"},
                {"type": "image_url", "image_url": {"url": "https://example.com/image.jpg"}}
            ]
        });

        transform_message_content_to_anthropic(message.as_object_mut().unwrap()).unwrap();

        let content = message["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "url");
    }

    #[test]
    fn test_transform_request_tool_result_message() {
        let transformer = OpenAiToAnthropicTransformer;

        let openai_request = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "assistant", "content": "Calling tool"},
                {
                    "role": "tool",
                    "tool_call_id": "call_123",
                    "content": "tool output"
                }
            ],
            "max_tokens": 1000
        });

        let result = transformer.transform_request(openai_request).unwrap();
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"][0]["type"], "tool_result");
        assert_eq!(messages[1]["content"][0]["tool_use_id"], "call_123");
        assert_eq!(messages[1]["content"][0]["content"], "tool output");
    }

    #[test]
    fn test_transform_request_assistant_tool_calls_to_tool_use() {
        let transformer = OpenAiToAnthropicTransformer;

        let openai_request = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc123",
                        "type": "function",
                        "function": {
                            "name": "calculator",
                            "arguments": "{\"a\":1,\"b\":2}"
                        }
                    }]
                }
            ],
            "max_tokens": 1000
        });

        let result = transformer.transform_request(openai_request).unwrap();
        let messages = result["messages"].as_array().unwrap();
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "tool_use");
        assert_eq!(content[0]["id"], "call_abc123");
        assert_eq!(content[0]["name"], "calculator");
        assert_eq!(content[0]["input"]["a"], 1);
        assert_eq!(content[0]["input"]["b"], 2);
    }

    #[test]
    fn test_extract_system_messages_content_blocks() {
        let mut request = serde_json::json!({
            "messages": [
                {
                    "role": "system",
                    "content": [
                        {"type": "text", "text": "Policy A"},
                        {"type": "text", "text": "Policy B"}
                    ]
                },
                {"role": "user", "content": "Hello"}
            ]
        });

        let system = extract_system_messages(request.as_object_mut().unwrap()).unwrap();
        assert_eq!(system, "Policy A\n\nPolicy B");
    }
}
