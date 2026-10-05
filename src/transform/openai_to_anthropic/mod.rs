// SPDX-License-Identifier: AGPL-3.0-or-later
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

mod helpers;
use helpers::*;

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
                *messages_array = normalize_anthropic_messages(transformed_messages);
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
                {
                    "role": "assistant",
                    "content": "Calling tool",
                    "tool_calls": [{
                        "id": "call_123",
                        "type": "function",
                        "function": {"name": "tool", "arguments": "{}"}
                    }]
                },
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
    fn test_transform_request_merges_parallel_tool_turns_and_drops_blank_assistant() {
        let transformer = OpenAiToAnthropicTransformer;

        let openai_request = serde_json::json!({
            "messages": [
                {"role": "user", "content": "Check state"},
                {"role": "assistant", "content": "Checking"},
                {"role": "assistant", "content": ""},
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "first", "arguments": "{}"}
                    }]
                },
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_2",
                        "type": "function",
                        "function": {"name": "second", "arguments": "{}"}
                    }]
                },
                {"role": "tool", "tool_call_id": "call_1", "content": "one"},
                {"role": "tool", "tool_call_id": "call_2", "content": "two"},
                {"role": "assistant", "content": ""},
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_3",
                        "type": "function",
                        "function": {"name": "third", "arguments": "{}"}
                    }]
                },
                {"role": "tool", "tool_call_id": "call_3", "content": "three"},
                {"role": "assistant", "content": "Finished"}
            ]
        });

        let result = transformer.transform_request(openai_request).unwrap();
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 6);
        assert_eq!(messages[0]["role"], "user");

        let assistant = &messages[1];
        assert_eq!(assistant["role"], "assistant");
        let blocks = assistant["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "Checking");
        let tool_ids: Vec<_> = blocks[1..]
            .iter()
            .map(|block| block["id"].as_str().unwrap())
            .collect();
        assert_eq!(tool_ids, vec!["call_1", "call_2"]);

        let results = &messages[2];
        assert_eq!(results["role"], "user");
        let result_ids: Vec<_> = results["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|block| block["tool_use_id"].as_str().unwrap())
            .collect();
        assert_eq!(result_ids, vec!["call_1", "call_2"]);
        assert_eq!(messages[3]["role"], "assistant");
        assert_eq!(messages[3]["content"][0]["type"], "tool_use");
        assert_eq!(messages[3]["content"][0]["id"], "call_3");
        assert_eq!(messages[4]["role"], "user");
        assert_eq!(messages[4]["content"][0]["tool_use_id"], "call_3");
        assert_eq!(messages[5]["role"], "assistant");
        assert_eq!(messages[5]["content"][0]["text"], "Finished");
    }

    #[test]
    fn test_transform_request_repairs_missing_parallel_tool_result() {
        let transformer = OpenAiToAnthropicTransformer;
        let request = serde_json::json!({
            "messages": [
                {
                    "role": "assistant",
                    "tool_calls": [
                        {"id": "call_a", "type": "function", "function": {"name": "a", "arguments": "{}"}},
                        {"id": "call_b", "type": "function", "function": {"name": "b", "arguments": "{}"}}
                    ]
                },
                {"role": "tool", "tool_call_id": "call_a", "content": "real a"}
            ]
        });

        let result = transformer.transform_request(request).unwrap();
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        let results = messages[1]["content"].as_array().unwrap();
        assert_eq!(results[0]["tool_use_id"], "call_a");
        assert_eq!(results[0]["content"], "real a");
        assert_eq!(results[1]["tool_use_id"], "call_b");
        assert_eq!(results[1]["is_error"], true);
        assert!(results[1]["content"]
            .as_str()
            .unwrap()
            .contains("CCR_TOOL_RESULT_MISSING"));
    }

    #[test]
    fn test_transform_request_orders_tool_results_before_user_text() {
        let transformer = OpenAiToAnthropicTransformer;
        let request = serde_json::json!({
            "messages": [
                {
                    "role": "assistant",
                    "tool_calls": [
                        {"id": "call_a", "type": "function", "function": {"name": "a", "arguments": "{}"}},
                        {"id": "call_b", "type": "function", "function": {"name": "b", "arguments": "{}"}}
                    ]
                },
                {"role": "tool", "tool_call_id": "call_a", "content": "real a"},
                {"role": "user", "content": "continue"}
            ]
        });

        let result = transformer.transform_request(request).unwrap();
        let messages = result["messages"].as_array().unwrap();
        let blocks = messages[1]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "tool_result");
        assert_eq!(blocks[0]["tool_use_id"], "call_a");
        assert_eq!(blocks[1]["type"], "tool_result");
        assert_eq!(blocks[1]["tool_use_id"], "call_b");
        assert_eq!(blocks[1]["is_error"], true);
        assert_eq!(blocks[2]["type"], "text");
        assert_eq!(blocks[2]["text"], "continue");
    }

    #[test]
    fn test_transform_request_inserts_missing_result_before_next_assistant() {
        let transformer = OpenAiToAnthropicTransformer;
        let request = serde_json::json!({
            "messages": [
                {"role": "user", "content": "run"},
                {
                    "role": "assistant",
                    "tool_calls": [{"id": "call_missing", "type": "function", "function": {"name": "probe", "arguments": "{}"}}]
                },
                {"role": "assistant", "content": "done"}
            ]
        });

        let result = transformer.transform_request(request).unwrap();
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][0]["type"], "tool_use");
        assert_eq!(messages[1]["content"][1]["text"], "done");
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "call_missing");
        assert_eq!(messages[2]["content"][0]["is_error"], true);
    }

    #[test]
    fn test_transform_request_retains_orphan_tool_result_as_text() {
        let transformer = OpenAiToAnthropicTransformer;
        let request = serde_json::json!({
            "messages": [
                {"role": "user", "content": "history begins mid-turn"},
                {"role": "tool", "tool_call_id": "orphan", "content": "orphan output"}
            ]
        });

        let result = transformer.transform_request(request).unwrap();
        let messages = result["messages"].as_array().unwrap();
        let text = messages[0]["content"][1]["text"].as_str().unwrap();
        assert!(text.contains("Unmatched tool result orphan"));
        assert!(text.contains("orphan output"));
        assert!(!text.contains("\"type\":\"tool_result\""));
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
