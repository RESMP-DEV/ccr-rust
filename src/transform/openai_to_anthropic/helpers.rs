// SPDX-License-Identifier: AGPL-3.0-or-later
//! Helper functions for OpenAI → Anthropic request/response transformation.

use anyhow::{anyhow, Result};
use serde_json::Value;

/// Extract system messages from the messages array and combine them.
///
/// OpenAI: System messages are in the messages array with role="system"
/// Anthropic: System prompt is a top-level string field
///
/// Returns combined system content as a string (empty if no system messages).
pub(super) fn extract_system_messages(
    request_obj: &mut serde_json::Map<String, Value>,
) -> Result<String> {
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
pub(super) fn transform_message_content_to_anthropic(
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
pub(super) fn transform_tool_result_message_to_anthropic(
    message_obj: &mut serde_json::Map<String, Value>,
) {
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

/// Normalize OpenAI-style message fragments into Anthropic conversational turns.
///
/// Codex continuation history can contain blank assistant messages and one
/// assistant message per parallel tool call. Anthropic-compatible providers
/// require one assistant turn containing every tool_use and the immediately
/// following user turn containing every matching tool_result.
pub(super) fn normalize_anthropic_messages(messages: Vec<Value>) -> Vec<Value> {
    let mut normalized: Vec<Value> = Vec::with_capacity(messages.len());

    for message in messages {
        let Some(message_obj) = message.as_object() else {
            normalized.push(message);
            continue;
        };
        let role = message_obj
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();

        if role == "assistant" && is_blank_anthropic_message(message_obj) {
            continue;
        }

        let merge_with_last = normalized
            .last()
            .and_then(Value::as_object)
            .and_then(|last| last.get("role"))
            .and_then(Value::as_str)
            .is_some_and(|last_role| last_role == role);

        if merge_with_last {
            let last = normalized.last_mut().expect("last message was checked");
            merge_anthropic_message_content(last, message_obj);
        } else {
            normalized.push(Value::Object(message_obj.clone()));
        }
    }

    normalized
}

fn is_blank_anthropic_message(message: &serde_json::Map<String, Value>) -> bool {
    match message.get("content") {
        Some(Value::Array(blocks)) => blocks.iter().all(|block| {
            block.as_object().is_some_and(|obj| {
                obj.get("type").and_then(Value::as_str) == Some("text")
                    && obj
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|text| text.trim().is_empty())
            })
        }),
        Some(Value::String(text)) => text.trim().is_empty(),
        Some(Value::Null) | None => true,
        Some(_) => false,
    }
}

fn merge_anthropic_message_content(
    target: &mut Value,
    additional: &serde_json::Map<String, Value>,
) {
    let target_obj = target
        .as_object_mut()
        .expect("target message must be an object");
    let target_content = target_obj
        .entry("content".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !target_content.is_array() {
        *target_content = serde_json::json!([target_content.clone()]);
    }

    let Some(target_blocks) = target_content.as_array_mut() else {
        return;
    };
    match additional.get("content") {
        Some(Value::Array(blocks)) => target_blocks.extend(blocks.iter().cloned()),
        Some(Value::String(text)) if !text.is_empty() => target_blocks.push(serde_json::json!({
            "type": "text",
            "text": text
        })),
        Some(Value::Null) | None => {}
        Some(other) => target_blocks.push(other.clone()),
    }
}

/// Extract plain text from content that may be a string, block array, or object.
pub(super) fn extract_text_content(content: &Value) -> String {
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

pub(super) fn default_input_schema() -> Value {
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
pub(super) fn map_openai_finish_reason(openai_reason: &str) -> &str {
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
pub(super) fn transform_openai_content(message: &Value) -> Result<Option<Vec<Value>>> {
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
pub(super) fn convert_openai_content_block(block: &Value) -> Result<Value> {
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
pub(super) fn convert_openai_tool_call(tool_call: &Value) -> Option<Value> {
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
