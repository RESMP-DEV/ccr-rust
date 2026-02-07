//! Anthropic to OpenAI response transformer.
//!
//! Converts Anthropic API format responses to OpenAI API format.
//! Handles:
//! - Response structure transformation (message → choices array)
//! - Content format transformation (content blocks → string/objects)
//! - Finish reason mapping (end_turn/max_tokens/tool_use → stop/length/tool_calls)
//! - Tool use block conversion to tool_calls format
//! - Streaming event transformation (SSE format conversion)

use crate::transformer::Transformer;
use anyhow::{anyhow, Result};
use serde_json::Value;
use tracing::debug;

/// Anthropic to OpenAI response transformer.
///
/// Converts Anthropic API format responses to OpenAI /v1/chat/completions format.
/// This handles the response transformation (reverse of OpenaiToAnthropicTransformer).
///
/// Transformations:
/// - `message` fields → `choices[0].message` structure
/// - Content blocks → `choices[0].message.content` (string or array)
/// - `stop_reason` mapping: end_turn→stop, max_tokens→length, tool_use→tool_calls
/// - `tool_use` blocks → `tool_calls` array
/// - Streaming events → OpenAI SSE format
#[derive(Debug, Clone)]
pub struct AnthropicToOpenAiResponseTransformer;

impl Transformer for AnthropicToOpenAiResponseTransformer {
    fn name(&self) -> &str {
        "anthropic-to-openai-response"
    }

    fn transform_response(&self, anthropic_response: Value) -> Result<Value> {
        // Handle streaming event format
        if anthropic_response.get("type").and_then(|t| t.as_str()) == Some("message_start")
            || anthropic_response.get("type").and_then(|t| t.as_str())
                == Some("content_block_start")
            || anthropic_response.get("type").and_then(|t| t.as_str())
                == Some("content_block_delta")
            || anthropic_response.get("type").and_then(|t| t.as_str()) == Some("message_delta")
            || anthropic_response.get("type").and_then(|t| t.as_str()) == Some("message_stop")
        {
            return transform_streaming_event(anthropic_response);
        }

        // Handle non-streaming response format
        transform_non_streaming_response(anthropic_response)
    }
}

/// Transform a non-streaming Anthropic response to OpenAI format.
fn transform_non_streaming_response(anthropic_response: Value) -> Result<Value> {
    let id = anthropic_response
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("chatcmpl-unknown");

    let model = anthropic_response
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let anthropic_stop_reason = anthropic_response
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("end_turn");

    let finish_reason = map_anthropic_stop_reason(anthropic_stop_reason);

    // Transform content blocks
    let (content, tool_calls) = transform_anthropic_content(&anthropic_response)?;

    // Build OpenAI format response
    let mut message = serde_json::json!({
        "role": "assistant",
        "content": content
    });

    // Add tool_calls if present
    if let Some(tools) = tool_calls {
        message["tool_calls"] = tools;
    }

    let openai_response = serde_json::json!({
        "id": id,
        "object": "chat.completion",
        "created": current_timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason
        }],
    });

    // Add usage if present
    let mut response_obj = openai_response;
    if let Some(usage) = anthropic_response.get("usage") {
        response_obj["usage"] = usage.clone();
    }

    debug!(
        from = anthropic_stop_reason,
        to = finish_reason,
        "transformed Anthropic response to OpenAI format"
    );

    Ok(response_obj)
}

/// Map Anthropic stop_reason to OpenAI finish_reason.
///
/// Mappings:
/// - `end_turn` → `stop`
/// - `max_tokens` → `length`
/// - `tool_use` → `tool_calls`
/// - `stop_sequence` → `stop`
fn map_anthropic_stop_reason(anthropic_reason: &str) -> &str {
    match anthropic_reason {
        "end_turn" => "stop",
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        "stop_sequence" => "stop",
        _ => "stop", // Default fallback
    }
}

/// Transform Anthropic content blocks to OpenAI format.
///
/// Returns a tuple of (content, tool_calls):
/// - content: String for text, or Array for multimodal, or Null
/// - tool_calls: Option<Value> containing the tool_calls array
fn transform_anthropic_content(anthropic_response: &Value) -> Result<(Value, Option<Value>)> {
    let content_blocks = anthropic_response
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| anyhow!("Anthropic response missing 'content' array"))?;

    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut has_multimodal = false;

    for block in content_blocks {
        let block_type = block
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Content block missing 'type'"))?;

        match block_type {
            "text" => {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    text_parts.push(text.to_string());
                }
            }
            "tool_use" => {
                if let Some(tool_call) = convert_anthropic_tool_use(block) {
                    tool_calls.push(tool_call);
                }
            }
            "thinking" => {
                // Skip thinking blocks in the main content
                // They could be exposed via a separate field if needed
            }
            "image" => {
                has_multimodal = true;
                // For multimodal, we need to return array format
                // This is handled below
            }
            _ => {
                // Unknown block type, skip
            }
        }
    }

    // Build content
    let content = if has_multimodal {
        // For multimodal content, convert to OpenAI format array
        convert_multimodal_content(content_blocks)?
    } else {
        // Simple text content - join all text parts
        let combined_text = text_parts.join("\n\n");
        Value::String(combined_text)
    };

    let tool_calls_value = if tool_calls.is_empty() {
        None
    } else {
        Some(Value::Array(tool_calls))
    };

    Ok((content, tool_calls_value))
}

/// Convert an Anthropic tool_use block to OpenAI tool_calls format.
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
///
/// OpenAI format:
/// ```json
/// {
///   "index": 0,
///   "id": "call_abc123",
///   "type": "function",
///   "function": {
///     "name": "tool_name",
///     "arguments": "{\"key\": \"value\"}"
///   }
/// }
/// ```
fn convert_anthropic_tool_use(tool_use: &Value) -> Option<Value> {
    let id = tool_use
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("toolu_unknown");

    let name = tool_use.get("name").and_then(|v| v.as_str())?;

    let input = tool_use
        .get("input")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    // Convert input object to JSON string
    let arguments = serde_json::to_string(&input).ok()?;

    Some(serde_json::json!({
        "id": id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": arguments
        }
    }))
}

/// Convert multimodal content blocks to OpenAI format.
fn convert_multimodal_content(content_blocks: &[Value]) -> Result<Value> {
    let mut openai_blocks: Vec<Value> = Vec::new();

    for block in content_blocks {
        let block_type = block
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Content block missing 'type'"))?;

        match block_type {
            "text" => {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    openai_blocks.push(serde_json::json!({
                        "type": "text",
                        "text": text
                    }));
                }
            }
            "image" => {
                // Convert Anthropic image format to OpenAI image_url format
                if let Some(source) = block.get("source") {
                    let image_url = if let Some(data) = source.get("data").and_then(|v| v.as_str())
                    {
                        // Base64 encoded data
                        let media_type = source
                            .get("media_type")
                            .and_then(|m| m.as_str())
                            .unwrap_or("image/jpeg");
                        format!("data:{};base64,{}", media_type, data)
                    } else if let Some(url) = source.get("url").and_then(|v| v.as_str()) {
                        // URL-based source
                        url.to_string()
                    } else {
                        continue;
                    };

                    openai_blocks.push(serde_json::json!({
                        "type": "image_url",
                        "image_url": {"url": image_url}
                    }));
                }
            }
            _ => {
                // Skip other block types for multimodal
            }
        }
    }

    Ok(Value::Array(openai_blocks))
}

/// Transform a streaming event from Anthropic to OpenAI SSE format.
///
/// Anthropic streaming events:
/// - `message_start`: Initial message metadata
/// - `content_block_start`: Start of a content block
/// - `content_block_delta`: Delta within a content block
/// - `content_block_stop`: End of a content block
/// - `message_delta`: Message-level delta
/// - `message_stop`: End of message
///
/// OpenAI streaming format:
/// - `chat.completion.chunk` with delta
fn transform_streaming_event(anthropic_event: Value) -> Result<Value> {
    let event_type = anthropic_event
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let openai_event = match event_type {
        "message_start" => {
            // Initial message metadata - create chunk with role
            let message = anthropic_event
                .get("message")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));

            serde_json::json!({
                "id": message.get("id").and_then(|v| v.as_str()).unwrap_or("chatcmpl-unknown"),
                "object": "chat.completion.chunk",
                "created": current_timestamp(),
                "model": message.get("model").and_then(|v| v.as_str()).unwrap_or("unknown"),
                "choices": [{
                    "index": 0,
                    "delta": {"role": "assistant"},
                    "finish_reason": null
                }]
            })
        }
        "content_block_start" => {
            // Start of a content block
            let content_block = anthropic_event
                .get("content_block")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));

            let block_type = content_block
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("text");

            let delta = match block_type {
                "text" => {
                    serde_json::json!({"content": content_block.get("text").and_then(|v| v.as_str()).unwrap_or("")})
                }
                "tool_use" => {
                    // Tool use start - emit tool_calls array
                    let id = content_block
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("toolu_unknown");
                    let name = content_block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    serde_json::json!({
                        "tool_calls": [{
                            "index": 0,
                            "id": id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": ""
                            }
                        }]
                    })
                }
                _ => serde_json::json!({}),
            };

            serde_json::json!({
                "id": "chatcmpl-stream",
                "object": "chat.completion.chunk",
                "created": current_timestamp(),
                "model": "unknown",
                "choices": [{
                    "index": 0,
                    "delta": delta,
                    "finish_reason": null
                }]
            })
        }
        "content_block_delta" => {
            // Delta within a content block
            let delta = anthropic_event
                .get("delta")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));

            let openai_delta = if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                serde_json::json!({"content": text})
            } else if let Some(thinking) = delta.get("thinking").and_then(|v| v.as_str()) {
                serde_json::json!({"reasoning_content": thinking})
            } else if let Some(partial_json) = delta.get("partial_json").and_then(|v| v.as_str()) {
                // Tool use partial JSON
                serde_json::json!({
                    "tool_calls": [{
                        "index": 0,
                        "function": {
                            "arguments": partial_json
                        }
                    }]
                })
            } else {
                serde_json::json!({})
            };

            serde_json::json!({
                "id": "chatcmpl-stream",
                "object": "chat.completion.chunk",
                "created": current_timestamp(),
                "model": "unknown",
                "choices": [{
                    "index": 0,
                    "delta": openai_delta,
                    "finish_reason": null
                }]
            })
        }
        "message_delta" => {
            // Message-level delta (e.g., stop_reason)
            let delta = anthropic_event
                .get("delta")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));

            let stop_reason = delta
                .get("stop_reason")
                .and_then(|v| v.as_str())
                .map(map_anthropic_stop_reason);

            serde_json::json!({
                "id": "chatcmpl-stream",
                "object": "chat.completion.chunk",
                "created": current_timestamp(),
                "model": "unknown",
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": stop_reason
                }]
            })
        }
        "message_stop" | "content_block_stop" => {
            // End of message or content block - no delta needed
            serde_json::json!({
                "id": "chatcmpl-stream",
                "object": "chat.completion.chunk",
                "created": current_timestamp(),
                "model": "unknown",
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": null
                }]
            })
        }
        _ => {
            // Unknown event type - return empty delta
            serde_json::json!({
                "id": "chatcmpl-stream",
                "object": "chat.completion.chunk",
                "created": current_timestamp(),
                "model": "unknown",
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": null
                }]
            })
        }
    };

    Ok(openai_event)
}

/// Get current Unix timestamp.
fn current_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transformer_name() {
        let transformer = AnthropicToOpenAiResponseTransformer;
        assert_eq!(transformer.name(), "anthropic-to-openai-response");
    }

    #[test]
    fn test_map_anthropic_stop_reason() {
        assert_eq!(map_anthropic_stop_reason("end_turn"), "stop");
        assert_eq!(map_anthropic_stop_reason("max_tokens"), "length");
        assert_eq!(map_anthropic_stop_reason("tool_use"), "tool_calls");
        assert_eq!(map_anthropic_stop_reason("stop_sequence"), "stop");
        assert_eq!(map_anthropic_stop_reason("unknown"), "stop");
    }

    #[test]
    fn test_transform_simple_text_response() {
        let transformer = AnthropicToOpenAiResponseTransformer;

        let anthropic_response = serde_json::json!({
            "id": "msg_abc123",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-opus-20240229",
            "content": [
                {"type": "text", "text": "Hello, world!"}
            ],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5
            }
        });

        let result = transformer.transform_response(anthropic_response).unwrap();

        assert_eq!(result["id"], "msg_abc123");
        assert_eq!(result["object"], "chat.completion");
        assert_eq!(result["model"], "claude-3-opus-20240229");
        assert_eq!(result["choices"][0]["message"]["role"], "assistant");
        assert_eq!(result["choices"][0]["message"]["content"], "Hello, world!");
        assert_eq!(result["choices"][0]["finish_reason"], "stop");
        assert_eq!(result["usage"]["input_tokens"], 10);
    }

    #[test]
    fn test_transform_tool_use_response() {
        let transformer = AnthropicToOpenAiResponseTransformer;

        let anthropic_response = serde_json::json!({
            "id": "msg_tool123",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-opus-20240229",
            "content": [
                {"type": "text", "text": "I'll calculate that for you."},
                {
                    "type": "tool_use",
                    "id": "toolu_01A",
                    "name": "calculator",
                    "input": {"operation": "add", "a": 1, "b": 2}
                }
            ],
            "stop_reason": "tool_use",
            "usage": {
                "input_tokens": 20,
                "output_tokens": 30
            }
        });

        let result = transformer.transform_response(anthropic_response).unwrap();

        assert_eq!(result["choices"][0]["finish_reason"], "tool_calls");
        let message = &result["choices"][0]["message"];
        assert_eq!(message["content"], "I'll calculate that for you.");

        let tool_calls = message["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "toolu_01A");
        assert_eq!(tool_calls[0]["type"], "function");
        assert_eq!(tool_calls[0]["function"]["name"], "calculator");

        // Arguments should be JSON string
        let args = tool_calls[0]["function"]["arguments"].as_str().unwrap();
        let args_json: Value = serde_json::from_str(args).unwrap();
        assert_eq!(args_json["operation"], "add");
        assert_eq!(args_json["a"], 1);
        assert_eq!(args_json["b"], 2);
    }

    #[test]
    fn test_transform_max_tokens_response() {
        let transformer = AnthropicToOpenAiResponseTransformer;

        let anthropic_response = serde_json::json!({
            "id": "msg_max123",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-opus-20240229",
            "content": [
                {"type": "text", "text": "This is incomplete..."}
            ],
            "stop_reason": "max_tokens",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 100
            }
        });

        let result = transformer.transform_response(anthropic_response).unwrap();

        assert_eq!(result["choices"][0]["finish_reason"], "length");
    }

    #[test]
    fn test_transform_streaming_message_start() {
        let transformer = AnthropicToOpenAiResponseTransformer;

        let anthropic_event = serde_json::json!({
            "type": "message_start",
            "message": {
                "id": "msg_stream123",
                "type": "message",
                "role": "assistant",
                "model": "claude-3-opus-20240229",
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {"input_tokens": 10, "output_tokens": 1}
            }
        });

        let result = transformer.transform_response(anthropic_event).unwrap();

        assert_eq!(result["object"], "chat.completion.chunk");
        assert_eq!(result["id"], "msg_stream123");
        assert_eq!(result["choices"][0]["delta"]["role"], "assistant");
        assert!(result["choices"][0]["finish_reason"].is_null());
    }

    #[test]
    fn test_transform_streaming_content_block_delta() {
        let transformer = AnthropicToOpenAiResponseTransformer;

        let anthropic_event = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {
                "type": "text_delta",
                "text": "Hello"
            }
        });

        let result = transformer.transform_response(anthropic_event).unwrap();

        assert_eq!(result["object"], "chat.completion.chunk");
        assert_eq!(result["choices"][0]["delta"]["content"], "Hello");
    }

    #[test]
    fn test_transform_streaming_tool_use_delta() {
        let transformer = AnthropicToOpenAiResponseTransformer;

        let anthropic_event = serde_json::json!({
            "type": "content_block_delta",
            "index": 1,
            "delta": {
                "type": "input_json_delta",
                "partial_json": "{\"a\": 1}"
            }
        });

        let result = transformer.transform_response(anthropic_event).unwrap();

        assert_eq!(result["object"], "chat.completion.chunk");
        let tool_calls = result["choices"][0]["delta"]["tool_calls"]
            .as_array()
            .unwrap();
        assert_eq!(tool_calls[0]["function"]["arguments"], "{\"a\": 1}");
    }

    #[test]
    fn test_transform_streaming_message_delta() {
        let transformer = AnthropicToOpenAiResponseTransformer;

        let anthropic_event = serde_json::json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": {"output_tokens": 50}
            },
            "usage": {"output_tokens": 50}
        });

        let result = transformer.transform_response(anthropic_event).unwrap();

        assert_eq!(result["object"], "chat.completion.chunk");
        assert_eq!(result["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn test_convert_anthropic_tool_use() {
        let tool_use = serde_json::json!({
            "type": "tool_use",
            "id": "toolu_123",
            "name": "weather",
            "input": {"city": "San Francisco", "units": "celsius"}
        });

        let result = convert_anthropic_tool_use(&tool_use).unwrap();

        assert_eq!(result["id"], "toolu_123");
        assert_eq!(result["type"], "function");
        assert_eq!(result["function"]["name"], "weather");

        let args = result["function"]["arguments"].as_str().unwrap();
        let args_json: Value = serde_json::from_str(args).unwrap();
        assert_eq!(args_json["city"], "San Francisco");
        assert_eq!(args_json["units"], "celsius");
    }

    #[test]
    fn test_multiple_text_blocks_joined() {
        let transformer = AnthropicToOpenAiResponseTransformer;

        let anthropic_response = serde_json::json!({
            "id": "msg_multi",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-opus-20240229",
            "content": [
                {"type": "text", "text": "First paragraph."},
                {"type": "text", "text": "Second paragraph."}
            ],
            "stop_reason": "end_turn",
            "usage": {}
        });

        let result = transformer.transform_response(anthropic_response).unwrap();

        assert_eq!(
            result["choices"][0]["message"]["content"],
            "First paragraph.\n\nSecond paragraph."
        );
    }
}
