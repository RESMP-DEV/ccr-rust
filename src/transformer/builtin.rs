// SPDX-License-Identifier: AGPL-3.0-or-later
//! Built-in concrete transformer implementations.
//!
//! These transformers handle format conversions between various LLM API formats
//! (Anthropic ↔ OpenAI), token limits, reasoning tags, and tool metadata.

use super::uuid_nopanic;
use super::Transformer;
use anyhow::Result;
use regex::Regex;
use serde_json::Value;

/// Anthropic to OpenAI transformer.
///
/// Converts Anthropic API format to OpenAI API format.
/// Handles:
/// - Message content blocks (array → string or keep array for multimodal)
/// - Tool definitions (input_schema → parameters)
/// - Tool choice (type conversion)
/// - Remove Anthropic-specific fields (metadata, stop_sequences)
#[derive(Debug, Clone)]
pub struct AnthropicToOpenaiTransformer;

impl Transformer for AnthropicToOpenaiTransformer {
    fn name(&self) -> &str {
        "anthropic-to-openai"
    }

    fn transform_request(&self, mut request: Value) -> Result<Value> {
        // Extract and handle the system field (Anthropic-specific)
        // Anthropic: system field at top level
        // OpenAI: system message prepended to messages array
        if let Some(request_obj) = request.as_object_mut() {
            if let Some(system) = request_obj.remove("system") {
                // Convert system prompt to string (Anthropic allows string or array of blocks)
                let system_content: String = if let Some(s) = system.as_str() {
                    s.to_string()
                } else if let Some(blocks) = system.as_array() {
                    // Handle array of content blocks
                    let text_blocks: Vec<&str> = blocks
                        .iter()
                        .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                        .collect();
                    text_blocks.join("\n\n")
                } else {
                    // Unknown format, convert to string
                    system.to_string()
                };

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
                                        .filter_map(|block| {
                                            block.get("text").and_then(|t| t.as_str())
                                        })
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
                                                    let image_url =
                                                        if let Some(data) = source.get("data") {
                                                            // Base64 encoded data
                                                            let media_type = source
                                                                .get("media_type")
                                                                .and_then(|m| m.as_str())
                                                                .unwrap_or("image/jpeg");
                                                            format!(
                                                                "data:{};base64,{}",
                                                                media_type,
                                                                data.as_str().unwrap_or("")
                                                            )
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
                    map.get("name").cloned().map(|name| {
                        serde_json::json!({
                            "type": "function",
                            "function": {"name": name}
                        })
                    })
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
                        if block_obj.get("type") == Some(&Value::String("tool_use".to_string()))
                            && !block_obj.contains_key("id")
                        {
                            block_obj.insert(
                                "id".to_string(),
                                Value::String(format!("toolu_{}", uuid_nopanic::timestamp_ms())),
                            );
                        }
                    }
                }
            }
        }
        Ok(response)
    }
}

/// Tool use enhancement transformer.
///
/// Ensures tool calls are properly formatted and adds any missing metadata.
#[derive(Debug, Clone)]
pub struct ToolUseTransformer;

impl Transformer for ToolUseTransformer {
    fn name(&self) -> &str {
        "tooluse"
    }

    fn transform_request(&self, mut request: Value) -> Result<Value> {
        // Ensure tools are properly formatted for providers that need it
        if let Some(tools_array) = request.get_mut("tools").and_then(|t| t.as_array_mut()) {
            for tool in tools_array {
                if let Some(tool_obj) = tool.as_object_mut() {
                    // Ensure tool has required fields
                    if !tool_obj.contains_key("input_schema") {
                        tool_obj.insert(
                            "input_schema".to_string(),
                            Value::Object(serde_json::Map::new()),
                        );
                    }
                }
            }
        }
        Ok(request)
    }

    fn transform_response(&self, mut response: Value) -> Result<Value> {
        // Ensure tool_use blocks have IDs
        if let Some(content_array) = response.get_mut("content").and_then(|c| c.as_array_mut()) {
            for block in content_array {
                if let Some(block_obj) = block.as_object_mut() {
                    if block_obj.get("type") == Some(&Value::String("tool_use".to_string()))
                        && !block_obj.contains_key("id")
                    {
                        block_obj.insert(
                            "id".to_string(),
                            Value::String(format!("toolu_{}", uuid_nopanic::timestamp_ms())),
                        );
                    }
                }
            }
        }
        Ok(response)
    }
}

/// Max tokens transformer.
///
/// Ensures requests respect max_tokens limits.
#[derive(Debug, Clone)]
pub struct MaxTokenTransformer {
    max_tokens: u32,
}

impl MaxTokenTransformer {
    pub fn new(max_tokens: u32) -> Self {
        Self { max_tokens }
    }
}

impl Transformer for MaxTokenTransformer {
    fn name(&self) -> &str {
        "maxtoken"
    }

    fn transform_request(&self, mut request: Value) -> Result<Value> {
        // Cap max_tokens if present
        if let Some(max_tokens) = request.get_mut("max_tokens") {
            if let Some(current) = max_tokens.as_u64() {
                if current > self.max_tokens as u64 {
                    *max_tokens = Value::Number(serde_json::Number::from(self.max_tokens));
                }
            }
        } else {
            // Add max_tokens if not present
            request
                .as_object_mut()
                .map(|obj| obj.insert("max_tokens".to_string(), self.max_tokens.into()));
        }
        Ok(request)
    }
}

/// Reasoning transformer for DeepSeek-R1 and other reasoning models.
///
/// Handles the special reasoning_content field in responses.
#[derive(Debug, Clone)]
pub struct ReasoningTransformer;

impl Transformer for ReasoningTransformer {
    fn name(&self) -> &str {
        "reasoning"
    }

    fn transform_response(&self, mut response: Value) -> Result<Value> {
        // Extract thinking field before mutable borrow of content
        let thinking_text = response
            .get("thinking")
            .and_then(|t| t.as_str())
            .map(String::from);

        // Ensure reasoning content is properly formatted in content blocks
        if let Some(content) = response.get_mut("content") {
            if let Some(content_array) = content.as_array_mut() {
                // Check if there's a thinking block
                let has_thinking = content_array
                    .iter()
                    .any(|block| block.get("type") == Some(&Value::String("thinking".to_string())));

                // Some providers return reasoning in a different format
                if !has_thinking {
                    if let Some(text) = thinking_text {
                        // Convert to proper thinking content block
                        let thinking_block = serde_json::json!({
                            "type": "thinking",
                            "thinking": text,
                            "signature": ""
                        });
                        content_array.insert(0, thinking_block);
                    }
                }
            }
        }
        Ok(response)
    }
}

/// Enhance tool transformer.
///
/// Adds additional metadata to tool calls for better handling.
#[derive(Debug, Clone)]
pub struct EnhanceToolTransformer;

impl Transformer for EnhanceToolTransformer {
    fn name(&self) -> &str {
        "enhancetool"
    }

    fn transform_response(&self, mut response: Value) -> Result<Value> {
        // Add cache_control metadata to tool_use blocks
        if let Some(content) = response.get_mut("content") {
            if let Some(content_array) = content.as_array_mut() {
                for block in content_array {
                    if let Some(block_obj) = block.as_object_mut() {
                        if block_obj.get("type") == Some(&Value::String("tool_use".to_string())) {
                            // Add cache control if missing
                            if !block_obj.contains_key("cache_control") {
                                let mut cache_control = serde_json::Map::new();
                                cache_control.insert(
                                    "type".to_string(),
                                    Value::String("ephemeral".to_string()),
                                );
                                block_obj.insert(
                                    "cache_control".to_string(),
                                    Value::Object(cache_control),
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

/// Think tag transformer.
///
/// Strips thinking/reasoning tags from response text content.
/// Removes <think>, <thinking>, and <reasoning> blocks and their content.
#[derive(Debug, Clone)]
pub struct ThinkTagTransformer;

impl Transformer for ThinkTagTransformer {
    fn name(&self) -> &str {
        "thinktag"
    }

    fn transform_response(&self, mut response: Value) -> Result<Value> {
        lazy_static::lazy_static! {
            // Regex crate doesn't support backreferences, so use alternation
            static ref THINK_TAG_RE: Regex = Regex::new(
                r"(?s)<think>.*?</think>|<thinking>.*?</thinking>|<reasoning>.*?</reasoning>"
            ).unwrap();
        }

        if let Some(content) = response.get_mut("content") {
            if let Some(arr) = content.as_array_mut() {
                for block in arr {
                    if let Some(text) = block.get_mut("text") {
                        if let Some(s) = text.as_str() {
                            let stripped = THINK_TAG_RE.replace_all(s, "");
                            *text = Value::String(stripped.trim().to_string());
                        }
                    }
                }
            }
        }
        Ok(response)
    }
}

/// LongCat thinking response normalizer.
///
/// LongCat's thinking model returns Anthropic-style `thinking` content blocks
/// without Anthropic's signed `signature` field. CCR-Rust cannot forward those
/// as real thinking blocks to Anthropic clients, and the OpenAI compatibility
/// layer cannot deserialize them before converting to `choices[]`. Preserve
/// normal text/tool output and only turn thinking into text when it is the only
/// useful content returned by the model.
#[derive(Debug, Clone)]
pub struct LongCatThinkingTransformer;

impl Transformer for LongCatThinkingTransformer {
    fn name(&self) -> &str {
        "longcat-thinking"
    }

    fn transform_response(&self, mut response: Value) -> Result<Value> {
        let Some(content_array) = response.get_mut("content").and_then(|c| c.as_array_mut()) else {
            return Ok(response);
        };

        let has_non_thinking = content_array.iter().any(|block| {
            block.get("type").and_then(|block_type| block_type.as_str()) != Some("thinking")
        });
        let original_blocks = std::mem::take(content_array);
        let mut normalized_blocks = Vec::with_capacity(original_blocks.len());

        for block in original_blocks {
            let is_unsigned_thinking = block.get("type").and_then(|block_type| block_type.as_str())
                == Some("thinking")
                && block.get("signature").is_none();

            if !is_unsigned_thinking {
                normalized_blocks.push(block);
                continue;
            }

            if has_non_thinking {
                continue;
            }

            if let Some(thinking) = block.get("thinking").and_then(|value| value.as_str()) {
                let text = thinking.trim();
                if !text.is_empty() {
                    normalized_blocks.push(serde_json::json!({
                        "type": "text",
                        "text": text
                    }));
                }
            }
        }

        *content_array = normalized_blocks;
        Ok(response)
    }
}
