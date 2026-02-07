//! Frontend implementation for OpenAI's Codex CLI API format.
//!
//! This frontend handles requests and responses that conform to the OpenAI
//! Chat Completions API specification, which is used by the Codex CLI.

use anyhow::Result;
use axum::http::HeaderMap;
use serde_json::Value;
use std::collections::VecDeque;

use crate::frontend::{ContentBlock, Frontend, InternalRequest, InternalResponse, Message, Tool};

/// Frontend for the OpenAI Codex CLI client.
#[derive(Debug, Clone, Default)]
pub struct CodexFrontend;

impl CodexFrontend {
    /// Creates a new CodexFrontend instance.
    pub fn new() -> Self {
        Self
    }
}

fn normalize_codex_role(role: &str) -> &str {
    match role {
        // OpenAI Responses API uses `developer`; many OpenAI-compatible
        // backends only accept `system`.
        "developer" => "system",
        _ => role,
    }
}

fn convert_openai_tool_call_to_tool_use(tool_call: &Value) -> Option<Value> {
    let function = tool_call.get("function")?;
    let name = function.get("name")?.as_str()?;
    let arguments = function
        .get("arguments")
        .and_then(|v| v.as_str())
        .unwrap_or("{}");
    let input = serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| serde_json::json!({}));
    let id = tool_call
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .unwrap_or("toolu_unknown");

    Some(serde_json::json!({
        "type": "tool_use",
        "id": id,
        "name": name,
        "input": input
    }))
}

fn convert_assistant_content_with_tool_calls(
    content: Option<&Value>,
    tool_calls: &[Value],
) -> Value {
    let mut blocks: Vec<Value> = Vec::new();

    if let Some(content) = content {
        match content {
            Value::String(text) if !text.is_empty() => {
                blocks.push(serde_json::json!({
                    "type": "text",
                    "text": text
                }));
            }
            Value::Array(items) => {
                for item in items {
                    if let Some(part_type) = item.get("type").and_then(|v| v.as_str()) {
                        match part_type {
                            "text" => {
                                blocks.push(serde_json::json!({
                                    "type": "text",
                                    "text": item.get("text").and_then(|v| v.as_str()).unwrap_or("")
                                }));
                            }
                            _ => blocks.push(item.clone()),
                        }
                    } else if let Some(text) = item.as_str() {
                        blocks.push(serde_json::json!({
                            "type": "text",
                            "text": text
                        }));
                    } else {
                        blocks.push(item.clone());
                    }
                }
            }
            Value::Null => {}
            other => {
                blocks.push(serde_json::json!({
                    "type": "text",
                    "text": other.to_string()
                }));
            }
        }
    }

    for tool_call in tool_calls {
        if let Some(tool_use) = convert_openai_tool_call_to_tool_use(tool_call) {
            blocks.push(tool_use);
        }
    }

    Value::Array(blocks)
}

impl Frontend for CodexFrontend {
    /// Returns the name of the frontend.
    fn name(&self) -> &str {
        "codex"
    }

    /// Detects if the request is in OpenAI/Codex format.
    ///
    /// Detection criteria:
    /// - User-Agent header contains "codex"
    /// - Request body has OpenAI-style messages with "role" field
    fn detect(&self, headers: &HeaderMap, body: &Value) -> bool {
        // Check for codex User-Agent
        if let Some(user_agent) = headers.get("user-agent").and_then(|v| v.to_str().ok()) {
            if user_agent.to_ascii_lowercase().contains("codex") {
                return true;
            }
        }

        // Check for OpenAI format: messages with "role" field
        if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
            // OpenAI format has messages with "role" (system/user/assistant)
            for message in messages {
                if message.get("role").is_some() {
                    return true;
                }
            }
        }

        false
    }

    /// Parses an incoming OpenAI request body into the internal normalized format.
    ///
    /// Converts OpenAI Chat Completions API format to InternalRequest.
    fn parse_request(&self, body: Value) -> Result<InternalRequest> {
        // Extract model
        let model = body
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("gpt-4")
            .to_string();

        // Parse messages
        let mut messages: Vec<Message> = Vec::new();
        let mut pending_tool_call_ids: VecDeque<String> = VecDeque::new();
        if let Some(msg_array) = body.get("messages").and_then(|m| m.as_array()) {
            for msg in msg_array {
                let role = msg
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("user")
                    .to_string();
                let role = normalize_codex_role(&role).to_string();

                let assistant_tool_calls = if role == "assistant" {
                    msg.get("tool_calls").and_then(|v| v.as_array())
                } else {
                    None
                };

                // Handle content - can be string or array (for multimodal). For assistant
                // tool-calling turns, normalize OpenAI tool_calls into Anthropic tool_use blocks.
                let content = if let Some(tool_calls) = assistant_tool_calls {
                    for tool_call in tool_calls {
                        if let Some(id) = tool_call.get("id").and_then(|v| v.as_str()) {
                            if !id.is_empty() {
                                pending_tool_call_ids.push_back(id.to_string());
                            }
                        }
                    }
                    convert_assistant_content_with_tool_calls(msg.get("content"), tool_calls)
                } else {
                    msg.get("content").cloned().unwrap_or(Value::Null)
                };

                // Extract tool_call_id for tool messages
                let explicit_tool_call_id = msg
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let tool_call_id = if role == "tool" {
                    if let Some(call_id) = explicit_tool_call_id {
                        if let Some(index) =
                            pending_tool_call_ids.iter().position(|id| id == &call_id)
                        {
                            pending_tool_call_ids.remove(index);
                        }
                        Some(call_id)
                    } else if pending_tool_call_ids.len() == 1 {
                        pending_tool_call_ids.pop_front()
                    } else {
                        None
                    }
                } else {
                    explicit_tool_call_id
                };

                messages.push(Message {
                    role,
                    content,
                    tool_call_id,
                });
            }
        }

        // Extract optional fields
        let max_tokens = body
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .or_else(|| {
                body.get("max_completion_tokens")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32)
            });

        let temperature = body
            .get("temperature")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32);

        let stream = body.get("stream").and_then(|v| v.as_bool());

        // Parse tools if present
        let tools = parse_openai_tools(&body)?;

        // Parse tool_choice if present
        let tool_choice = body.get("tool_choice").cloned();

        // Parse stop sequences if present
        let stop_sequences = body.get("stop").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        });

        // Collect extra params that are not standard
        let standard_keys: &[&str] = &[
            "model",
            "messages",
            "max_tokens",
            "max_completion_tokens",
            "temperature",
            "stream",
            "tools",
            "tool_choice",
            "stop",
            "top_p",
            "n",
            "presence_penalty",
            "frequency_penalty",
            "logit_bias",
            "user",
            "response_format",
            "seed",
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
            system: None, // OpenAI uses system message in messages array
            max_tokens,
            temperature,
            stream,
            tools,
            tool_choice,
            stop_sequences,
            extra_params,
        })
    }

    /// Serializes an internal response into OpenAI Chat Completions format.
    ///
    /// Converts InternalResponse to OpenAI JSON response format.
    fn serialize_response(&self, response: InternalResponse) -> Result<Vec<u8>> {
        // Extract text content from content blocks
        let mut content = String::new();
        let mut tool_calls: Vec<Value> = Vec::new();

        for block in &response.content {
            match block {
                ContentBlock::Text { text } => {
                    content.push_str(text);
                }
                ContentBlock::ToolUse { id, name, input } => {
                    // Convert to OpenAI tool_calls format
                    tool_calls.push(serde_json::json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": input.to_string()
                        }
                    }));
                }
                ContentBlock::ToolResult { .. } => {
                    // Tool results are typically in user messages, not assistant responses
                }
                ContentBlock::Image { .. } => {
                    // Images in response are not standard in OpenAI chat completions
                }
                ContentBlock::Thinking { thinking, .. } => {
                    // Include thinking content wrapped in tags
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str("<thinking>");
                    content.push_str(thinking);
                    content.push_str("</thinking>");
                }
            }
        }

        // Map stop_reason from internal to OpenAI format
        let finish_reason = response.stop_reason.as_deref().map(|reason| {
            match reason {
                "end_turn" => "stop",
                "max_tokens" => "length",
                "tool_use" => "tool_calls",
                "stop_sequence" => "stop",
                other => other,
            }
            .to_string()
        });

        // Build the message object
        let mut message = serde_json::json!({
            "role": response.role,
            "content": content
        });

        // Add tool_calls if present
        if !tool_calls.is_empty() {
            message["tool_calls"] = Value::Array(tool_calls);
        }

        // Build the OpenAI response
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let mut openai_response = serde_json::json!({
            "id": response.id,
            "object": "chat.completion",
            "created": now,
            "model": response.model,
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": finish_reason.unwrap_or_else(|| "stop".to_string())
            }]
        });

        // Add usage if available
        if let Some(usage) = &response.usage {
            openai_response["usage"] = serde_json::json!({
                "prompt_tokens": usage.input_tokens,
                "completion_tokens": usage.output_tokens,
                "total_tokens": usage.input_tokens + usage.output_tokens
            });
        }

        // Add extra data if present
        if let Some(extra) = &response.extra_data {
            if let Some(obj) = extra.as_object() {
                for (key, value) in obj {
                    if openai_response.get(key).is_none() {
                        openai_response[key] = value.clone();
                    }
                }
            }
        }

        serde_json::to_vec(&openai_response).map_err(anyhow::Error::from)
    }
}

/// Parse OpenAI-style tools into internal Tool format.
///
/// OpenAI format:
/// ```json
/// {
///   "type": "function",
///   "function": {
///     "name": "tool_name",
///     "description": "...",
///     "parameters": { ... }
///   }
/// }
/// ```
fn parse_openai_tools(body: &Value) -> Result<Option<Vec<Tool>>> {
    let tools = match body.get("tools") {
        Some(Value::Array(tools_array)) => {
            let mut tools: Vec<Tool> = Vec::new();
            for tool_value in tools_array {
                let tool_type = tool_value
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("function");

                if tool_type != "function" {
                    continue; // Skip non-function tools for now
                }

                // Be tolerant: some clients send tool definitions without a nested
                // `function` wrapper. Treat the top-level object as function config.
                let function = tool_value.get("function").unwrap_or(tool_value);

                let name = match function.get("name").and_then(|v| v.as_str()) {
                    Some(n) if !n.is_empty() => n.to_string(),
                    _ => continue, // Skip malformed tool entries
                };

                let description = function
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let input_schema = function.get("parameters").cloned();

                tools.push(Tool {
                    name,
                    description,
                    input_schema,
                });
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
    use axum::http::HeaderMap;
    use serde_json::json;

    fn create_frontend() -> CodexFrontend {
        CodexFrontend::new()
    }

    #[test]
    fn test_name() {
        let frontend = create_frontend();
        assert_eq!(frontend.name(), "codex");
    }

    #[test]
    fn test_detect_codex_user_agent() {
        let frontend = create_frontend();
        let mut headers = HeaderMap::new();
        headers.insert("user-agent", "codex-cli/1.0.0".parse().unwrap());

        let body = json!({});
        assert!(frontend.detect(&headers, &body));
    }

    #[test]
    fn test_detect_openai_format() {
        let frontend = create_frontend();
        let headers = HeaderMap::new();
        let body = json!({
            "messages": [
                {"role": "system", "content": "You are helpful"},
                {"role": "user", "content": "Hi"}
            ]
        });

        assert!(frontend.detect(&headers, &body));
    }

    #[test]
    fn test_detect_not_codex_format() {
        let frontend = create_frontend();
        let headers = HeaderMap::new();
        // Pure Anthropic format: has top-level model but messages without role field
        let body = json!({
            "model": "claude-3-opus",
            "messages": [{"content": "Hi"}]
        });

        // Anthropic format without role fields should not be detected as Codex
        assert!(!frontend.detect(&headers, &body));
    }

    #[test]
    fn test_parse_request_simple() {
        let frontend = create_frontend();
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are helpful"},
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 100,
            "temperature": 0.7
        });

        let request = frontend.parse_request(body).unwrap();

        assert_eq!(request.model, "gpt-4");
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, "system");
        assert_eq!(request.messages[1].role, "user");
        assert_eq!(request.max_tokens, Some(100));
        assert_eq!(request.temperature, Some(0.7));
    }

    #[test]
    fn test_parse_request_normalizes_developer_role() {
        let frontend = create_frontend();
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "developer", "content": "Policy"},
                {"role": "user", "content": "Hello"}
            ]
        });

        let request = frontend.parse_request(body).unwrap();
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, "system");
        assert_eq!(request.messages[1].role, "user");
    }

    #[test]
    fn test_parse_request_with_tools() {
        let frontend = create_frontend();
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Calculate 2+2"}],
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "calculator",
                        "description": "Performs calculations",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "expression": {"type": "string"}
                            }
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
        assert_eq!(
            tools[0].description,
            Some("Performs calculations".to_string())
        );
    }

    #[test]
    fn test_parse_request_with_tools_without_function_wrapper() {
        let frontend = create_frontend();
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Run tool"}],
            "tools": [
                {
                    "type": "function",
                    "name": "calculator",
                    "description": "Inline tool definition",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "expression": {"type": "string"}
                        }
                    }
                }
            ]
        });

        let request = frontend.parse_request(body).unwrap();
        let tools = request.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "calculator");
    }

    #[test]
    fn test_parse_request_skips_malformed_tool_entries() {
        let frontend = create_frontend();
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Run tool"}],
            "tools": [
                {
                    "type": "function",
                    "description": "Missing name should be ignored"
                }
            ]
        });

        let request = frontend.parse_request(body).unwrap();
        assert!(request.tools.is_none());
    }

    #[test]
    fn test_parse_request_converts_assistant_tool_calls_to_tool_use_blocks() {
        let frontend = create_frontend();
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {
                    "role": "assistant",
                    "content": "I will call a tool",
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "calculator",
                            "arguments": "{\"expression\":\"2+2\"}"
                        }
                    }]
                }
            ]
        });

        let request = frontend.parse_request(body).unwrap();
        assert_eq!(request.messages.len(), 1);
        let blocks = request.messages[0]
            .content
            .as_array()
            .expect("assistant content should be block array");
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "I will call a tool");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["id"], "call_abc");
        assert_eq!(blocks[1]["name"], "calculator");
        assert_eq!(blocks[1]["input"], json!({"expression": "2+2"}));
    }

    #[test]
    fn test_parse_request_infers_tool_call_id_when_unambiguous() {
        let frontend = create_frontend();
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "calculator",
                            "arguments": "{\"expression\":\"2+2\"}"
                        }
                    }]
                },
                {
                    "role": "tool",
                    "content": "4"
                }
            ]
        });

        let request = frontend.parse_request(body).unwrap();
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[1].role, "tool");
        assert_eq!(
            request.messages[1].tool_call_id.as_deref(),
            Some("call_abc")
        );
    }

    #[test]
    fn test_parse_request_does_not_infer_tool_call_id_when_ambiguous() {
        let frontend = create_frontend();
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_a",
                        "type": "function",
                        "function": {
                            "name": "first",
                            "arguments": "{}"
                        }
                    }, {
                        "id": "call_b",
                        "type": "function",
                        "function": {
                            "name": "second",
                            "arguments": "{}"
                        }
                    }]
                },
                {
                    "role": "tool",
                    "content": "result"
                }
            ]
        });

        let request = frontend.parse_request(body).unwrap();
        assert_eq!(request.messages.len(), 2);
        assert!(request.messages[1].tool_call_id.is_none());
    }

    #[test]
    fn test_serialize_response_simple() {
        let frontend = create_frontend();
        let response = InternalResponse {
            id: "chatcmpl-123".to_string(),
            response_type: "message".to_string(),
            role: "assistant".to_string(),
            model: "gpt-4".to_string(),
            content: vec![ContentBlock::Text {
                text: "Hello, world!".to_string(),
            }],
            stop_reason: Some("end_turn".to_string()),
            usage: Some(crate::frontend::Usage {
                input_tokens: 10,
                output_tokens: 5,
                input_tokens_details: None,
            }),
            extra_data: None,
        };

        let bytes = frontend.serialize_response(response).unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(json["id"], "chatcmpl-123");
        assert_eq!(json["object"], "chat.completion");
        assert_eq!(json["model"], "gpt-4");
        assert_eq!(json["choices"][0]["message"]["content"], "Hello, world!");
        assert_eq!(json["choices"][0]["finish_reason"], "stop");
        assert_eq!(json["usage"]["prompt_tokens"], 10);
        assert_eq!(json["usage"]["completion_tokens"], 5);
    }

    #[test]
    fn test_serialize_response_with_tool_calls() {
        let frontend = create_frontend();
        let response = InternalResponse {
            id: "chatcmpl-456".to_string(),
            response_type: "message".to_string(),
            role: "assistant".to_string(),
            model: "gpt-4".to_string(),
            content: vec![
                ContentBlock::Text {
                    text: "I'll calculate that.".to_string(),
                },
                ContentBlock::ToolUse {
                    id: "call_abc123".to_string(),
                    name: "calculator".to_string(),
                    input: json!({"expression": "2+2"}),
                },
            ],
            stop_reason: Some("tool_use".to_string()),
            usage: None,
            extra_data: None,
        };

        let bytes = frontend.serialize_response(response).unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(json["choices"][0]["finish_reason"], "tool_calls");
        assert!(json["choices"][0]["message"]["tool_calls"].is_array());
        assert_eq!(
            json["choices"][0]["message"]["tool_calls"][0]["type"],
            "function"
        );
        assert_eq!(
            json["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "calculator"
        );
    }

    #[test]
    fn test_serialize_response_max_tokens() {
        let frontend = create_frontend();
        let response = InternalResponse {
            id: "chatcmpl-789".to_string(),
            response_type: "message".to_string(),
            role: "assistant".to_string(),
            model: "gpt-4".to_string(),
            content: vec![ContentBlock::Text {
                text: "This was cut off...".to_string(),
            }],
            stop_reason: Some("max_tokens".to_string()),
            usage: None,
            extra_data: None,
        };

        let bytes = frontend.serialize_response(response).unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(json["choices"][0]["finish_reason"], "length");
    }
}
