//! Frontend implementation for Anthropic's Claude Code API format.
//!
//! This frontend handles requests and responses that conform to the Anthropic
//! Messages API specification, including features like `cache_control` and
//! `thinking` blocks.

use anyhow::Result;
use axum::http::HeaderMap;
use serde_json::Value;

use crate::frontend::{
    ContentBlock, Frontend, ImageSource, InternalRequest, InternalResponse, Message, Tool,
};

/// Frontend for the Anthropic Claude Code client.
#[derive(Debug, Clone, Default)]
pub struct ClaudeCodeFrontend;

impl ClaudeCodeFrontend {
    /// Creates a new ClaudeCodeFrontend instance.
    pub fn new() -> Self {
        Self
    }
}

impl Frontend for ClaudeCodeFrontend {
    /// Returns the name of the frontend.
    fn name(&self) -> &str {
        "claude_code"
    }

    /// Detects if the request is in Anthropic format.
    ///
    /// Detection criteria:
    /// - Headers starting with "anthropic-"
    /// - Body has "anthropic_version" field
    /// - Body has top-level "system" field
    /// - Messages have content as an array of blocks
    fn detect(&self, headers: &HeaderMap, body: &Value) -> bool {
        // Check for anthropic-* headers which are a strong signal.
        for key in headers.keys() {
            if key.as_str().starts_with("anthropic-") {
                return true;
            }
        }

        // Check for Anthropic-specific fields in the request body.
        if body.get("anthropic_version").is_some() || body.get("system").is_some() {
            return true;
        }

        // Anthropic messages may have content as an array of blocks.
        if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
            for message in messages {
                if let Some(content) = message.get("content") {
                    if content.is_array() {
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Parses an incoming request body into the internal normalized format.
    ///
    /// Converts Anthropic Messages API format to InternalRequest.
    fn parse_request(&self, body: Value) -> Result<InternalRequest> {
        // Extract model
        let model = body
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("claude-3-opus")
            .to_string();

        // Extract system prompt if present (Anthropic has top-level system)
        let system = body.get("system").cloned();

        // Parse messages
        let mut messages: Vec<Message> = Vec::new();
        if let Some(msg_array) = body.get("messages").and_then(|m| m.as_array()) {
            for msg in msg_array {
                let role = msg
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("user")
                    .to_string();

                // Handle content - can be string or array of content blocks
                let content = msg.get("content").cloned().unwrap_or(Value::Null);

                messages.push(Message {
                    role,
                    content,
                    tool_call_id: None, // Anthropic uses tool_use_id in content blocks
                });
            }
        }

        // Extract optional fields
        let max_tokens = body
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);

        let temperature = body
            .get("temperature")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32);

        let stream = body.get("stream").and_then(|v| v.as_bool());

        // Parse tools if present
        let tools = parse_anthropic_tools(&body)?;

        // Parse tool_choice if present
        let tool_choice = body.get("tool_choice").cloned();

        // Parse stop sequences if present
        let stop_sequences = body
            .get("stop_sequences")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            });

        // Collect extra params
        let standard_keys: &[&str] = &[
            "model",
            "messages",
            "system",
            "max_tokens",
            "temperature",
            "stream",
            "tools",
            "tool_choice",
            "stop_sequences",
            "anthropic_version",
            "metadata",
            "top_p",
            "top_k",
        ];
        let mut extra = serde_json::Map::new();
        if let Some(obj) = body.as_object() {
            for (key, value) in obj {
                if !standard_keys.contains(&key.as_str()) {
                    extra.insert(key.clone(), value.clone());
                }
            }
        }
        let extra_params = if extra.is_empty() {
            None
        } else {
            Some(Value::Object(extra))
        };

        Ok(InternalRequest {
            model,
            messages,
            system,
            max_tokens,
            temperature,
            stream,
            tools,
            tool_choice,
            stop_sequences,
            extra_params,
        })
    }

    /// Serializes an internal response into Anthropic Messages format.
    ///
    /// Converts InternalResponse to Anthropic JSON response format.
    fn serialize_response(&self, response: InternalResponse) -> Result<Vec<u8>> {
        // Convert content blocks to Anthropic format
        let content: Vec<Value> = response
            .content
            .into_iter()
            .map(|block| match block {
                ContentBlock::Text { text } => {
                    serde_json::json!({"type": "text", "text": text})
                }
                ContentBlock::Thinking {
                    thinking,
                    signature,
                } => {
                    let mut obj = serde_json::json!({
                        "type": "thinking",
                        "thinking": thinking
                    });
                    if let Some(sig) = signature {
                        obj["signature"] = Value::String(sig);
                    }
                    obj
                }
                ContentBlock::ToolUse { id, name, input } => {
                    serde_json::json!({
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": input
                    })
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                } => {
                    serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content
                    })
                }
                ContentBlock::Image { source } => match source {
                    ImageSource::Base64 { media_type, data } => {
                        serde_json::json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": media_type,
                                "data": data
                            }
                        })
                    }
                    ImageSource::Url { url } => {
                        serde_json::json!({
                            "type": "image",
                            "source": {
                                "type": "url",
                                "url": url
                            }
                        })
                    }
                },
            })
            .collect();

        // Map stop_reason to Anthropic format
        let stop_reason = response.stop_reason.map(|reason| match reason.as_str() {
            "stop" | "end_turn" => "end_turn".to_string(),
            "length" | "max_tokens" => "max_tokens".to_string(),
            "tool_calls" | "tool_use" => "tool_use".to_string(),
            "stop_sequence" => "stop_sequence".to_string(),
            other => other.to_string(),
        });

        // Build the Anthropic response
        let mut anthropic_response = serde_json::json!({
            "id": response.id,
            "type": "message",
            "role": response.role,
            "model": response.model,
            "content": content,
        });

        if let Some(stop) = stop_reason {
            anthropic_response["stop_reason"] = Value::String(stop);
        }

        // Add usage if available
        if let Some(usage) = response.usage {
            anthropic_response["usage"] = serde_json::json!({
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens
            });
        }

        // Add extra data if present
        if let Some(extra) = response.extra_data {
            if let Some(obj) = extra.as_object() {
                for (key, value) in obj {
                    if anthropic_response.get(key).is_none() {
                        anthropic_response[key] = value.clone();
                    }
                }
            }
        }

        serde_json::to_vec(&anthropic_response).map_err(anyhow::Error::from)
    }
}

/// Parse Anthropic-style tools into internal Tool format.
///
/// Anthropic format:
/// ```json
/// {
///   "name": "tool_name",
///   "description": "...",
///   "input_schema": { ... }
/// }
/// ```
fn parse_anthropic_tools(body: &Value) -> Result<Option<Vec<Tool>>> {
    let tools = match body.get("tools") {
        Some(Value::Array(tools_array)) => {
            let mut tools: Vec<Tool> = Vec::new();
            for tool_value in tools_array {
                let name = tool_value
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let description = tool_value
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let input_schema = tool_value.get("input_schema").cloned();

                if let Some(name) = name {
                    tools.push(Tool {
                        name,
                        description,
                        input_schema,
                    });
                }
            }
            if tools.is_empty() {
                None
            } else {
                Some(tools)
            }
        }
        _ => None,
    };

    Ok(tools)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::Usage;
    use axum::http::HeaderMap;
    use serde_json::json;

    fn create_frontend() -> ClaudeCodeFrontend {
        ClaudeCodeFrontend::new()
    }

    #[test]
    fn test_name() {
        let frontend = create_frontend();
        assert_eq!(frontend.name(), "claude_code");
    }

    #[test]
    fn test_detect_anthropic_headers() {
        let frontend = create_frontend();
        let mut headers = HeaderMap::new();
        headers.insert("anthropic-version", "2023-06-01".parse().unwrap());

        let body = json!({});
        assert!(frontend.detect(&headers, &body));
    }

    #[test]
    fn test_detect_anthropic_version_field() {
        let frontend = create_frontend();
        let headers = HeaderMap::new();
        let body = json!({
            "anthropic_version": "2023-06-01",
            "messages": [{"role": "user", "content": "Hi"}]
        });

        assert!(frontend.detect(&headers, &body));
    }

    #[test]
    fn test_detect_system_field() {
        let frontend = create_frontend();
        let headers = HeaderMap::new();
        let body = json!({
            "model": "claude-3-opus",
            "system": "You are helpful",
            "messages": [{"role": "user", "content": "Hi"}]
        });

        assert!(frontend.detect(&headers, &body));
    }

    #[test]
    fn test_detect_content_blocks() {
        let frontend = create_frontend();
        let headers = HeaderMap::new();
        let body = json!({
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "Hello"}]}
            ]
        });

        assert!(frontend.detect(&headers, &body));
    }

    #[test]
    fn test_detect_not_anthropic_format() {
        let frontend = create_frontend();
        let headers = HeaderMap::new();
        let body = json!({
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });

        // Simple string content without anthropic headers/fields should not be detected
        assert!(!frontend.detect(&headers, &body));
    }

    #[test]
    fn test_parse_request_simple() {
        let frontend = create_frontend();
        let body = json!({
            "model": "claude-3-opus",
            "messages": [
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 100,
            "temperature": 0.7
        });

        let request = frontend.parse_request(body).unwrap();

        assert_eq!(request.model, "claude-3-opus");
        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.messages[0].role, "user");
        assert_eq!(request.max_tokens, Some(100));
        assert_eq!(request.temperature, Some(0.7));
    }

    #[test]
    fn test_parse_request_with_system() {
        let frontend = create_frontend();
        let body = json!({
            "model": "claude-3-opus",
            "system": "You are helpful",
            "messages": [{"role": "user", "content": "Hello"}]
        });

        let request = frontend.parse_request(body).unwrap();

        assert!(request.system.is_some());
        assert_eq!(request.system.unwrap(), "You are helpful");
    }

    #[test]
    fn test_parse_request_with_tools() {
        let frontend = create_frontend();
        let body = json!({
            "model": "claude-3-opus",
            "messages": [{"role": "user", "content": "Hi"}],
            "tools": [
                {
                    "name": "calculator",
                    "description": "Performs calculations",
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "expression": {"type": "string"}
                        }
                    }
                }
            ]
        });

        let request = frontend.parse_request(body).unwrap();

        assert!(request.tools.is_some());
        let tools = request.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "calculator");
    }

    #[test]
    fn test_serialize_response_simple() {
        let frontend = create_frontend();
        let response = InternalResponse {
            id: "msg_01AbCdEfGhIjKlMn".to_string(),
            response_type: "message".to_string(),
            role: "assistant".to_string(),
            model: "claude-3-opus".to_string(),
            content: vec![ContentBlock::Text {
                text: "Hello, world!".to_string(),
            }],
            stop_reason: Some("end_turn".to_string()),
            usage: Some(Usage {
                input_tokens: 10,
                output_tokens: 5,
                input_tokens_details: None,
            }),
            extra_data: None,
        };

        let bytes = frontend.serialize_response(response).unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(json["id"], "msg_01AbCdEfGhIjKlMn");
        assert_eq!(json["type"], "message");
        assert_eq!(json["role"], "assistant");
        assert_eq!(json["model"], "claude-3-opus");
        assert_eq!(json["content"][0]["type"], "text");
        assert_eq!(json["content"][0]["text"], "Hello, world!");
        assert_eq!(json["stop_reason"], "end_turn");
    }

    #[test]
    fn test_serialize_response_with_thinking() {
        let frontend = create_frontend();
        let response = InternalResponse {
            id: "msg_01Xy".to_string(),
            response_type: "message".to_string(),
            role: "assistant".to_string(),
            model: "claude-3-opus".to_string(),
            content: vec![
                ContentBlock::Thinking {
                    thinking: "Let me think...".to_string(),
                    signature: Some("sig123".to_string()),
                },
                ContentBlock::Text {
                    text: "The answer is 42.".to_string(),
                },
            ],
            stop_reason: Some("end_turn".to_string()),
            usage: None,
            extra_data: None,
        };

        let bytes = frontend.serialize_response(response).unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(json["content"][0]["type"], "thinking");
        assert_eq!(json["content"][0]["thinking"], "Let me think...");
        assert_eq!(json["content"][0]["signature"], "sig123");
        assert_eq!(json["content"][1]["type"], "text");
        assert_eq!(json["content"][1]["text"], "The answer is 42.");
    }

    #[test]
    fn test_serialize_response_max_tokens() {
        let frontend = create_frontend();
        let response = InternalResponse {
            id: "msg_01Max".to_string(),
            response_type: "message".to_string(),
            role: "assistant".to_string(),
            model: "claude-3-opus".to_string(),
            content: vec![ContentBlock::Text {
                text: "This was cut off...".to_string(),
            }],
            stop_reason: Some("max_tokens".to_string()),
            usage: None,
            extra_data: None,
        };

        let bytes = frontend.serialize_response(response).unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(json["stop_reason"], "max_tokens");
    }
}
