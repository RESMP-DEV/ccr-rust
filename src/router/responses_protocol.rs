// SPDX-License-Identifier: AGPL-3.0-or-later
//! OpenAI Chat Completions to Responses API upstream conversion.

use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};

fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn function_arguments(arguments: Option<&Value>) -> String {
    let rendered = arguments.map(content_text).unwrap_or_default();
    if rendered.trim().is_empty() {
        "{}".to_string()
    } else {
        rendered
    }
}

fn response_created_at(created_at: Option<&Value>) -> i64 {
    created_at
        .and_then(|created| {
            created.as_i64().or_else(|| {
                created.as_str().and_then(|value| {
                    value.parse().ok().or_else(|| {
                        chrono::DateTime::parse_from_rfc3339(value)
                            .ok()
                            .map(|timestamp| timestamp.timestamp())
                    })
                })
            })
        })
        .unwrap_or_default()
}

fn response_content_blocks(content: &Value, role: &str) -> Result<Vec<Value>> {
    let text_type = if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };
    match content {
        Value::String(text) if text.is_empty() => Ok(Vec::new()),
        Value::String(text) => Ok(vec![json!({"type": text_type, "text": text})]),
        Value::Array(items) => {
            let mut blocks = Vec::new();
            for item in items {
                let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
                let block = match item_type {
                    "text" | "input_text" | "output_text" => {
                        let text = item.get("text").ok_or_else(|| {
                            anyhow!("Responses text content block is missing 'text'")
                        })?;
                        let text = text.as_str().ok_or_else(|| {
                            anyhow!("Responses text content block requires string 'text'")
                        })?;
                        (!text.is_empty()).then(|| json!({"type": text_type, "text": text}))
                    }
                    "image_url" => item
                        .get("image_url")
                        .and_then(|image| {
                            image
                                .as_str()
                                .or_else(|| image.get("url").and_then(Value::as_str))
                        })
                        .map(|image_url| json!({"type": "input_image", "image_url": image_url})),
                    "input_image" => Some(item.clone()),
                    _ => None,
                };
                if let Some(block) = block {
                    blocks.push(block);
                }
            }
            Ok(blocks)
        }
        Value::Null => Ok(Vec::new()),
        other => Ok(vec![json!({"type": text_type, "text": other.to_string()})]),
    }
}

fn responses_tool(tool: &Value) -> Option<Value> {
    let object = tool.as_object()?;
    let tool_type = object
        .get("type")
        .and_then(Value::as_str)
        .filter(|tool_type| !tool_type.is_empty())?;
    if tool_type != "function" {
        return Some(tool.clone());
    }
    if tool.get("name").and_then(Value::as_str).is_some() {
        return Some(tool.clone());
    }
    let function = tool.get("function")?.as_object()?;
    function.get("name")?.as_str()?;
    let mut converted = function.clone();
    converted.insert("type".to_string(), Value::String("function".to_string()));
    Some(Value::Object(converted))
}

fn responses_tool_choice(choice: &Value) -> Option<Value> {
    if choice.is_string() {
        return Some(choice.clone());
    }
    let object = choice.as_object()?;
    let choice_type = object
        .get("type")
        .and_then(Value::as_str)
        .filter(|choice_type| !choice_type.is_empty())?;
    if choice_type != "function" {
        return Some(choice.clone());
    }
    if choice.get("name").and_then(Value::as_str).is_some() {
        return Some(choice.clone());
    }
    let function = choice.get("function").and_then(Value::as_object)?;
    let name = function.get("name")?.as_str()?;
    let mut converted = Map::new();
    converted.insert("type".to_string(), Value::String("function".to_string()));
    converted.insert("name".to_string(), Value::String(name.to_string()));
    Some(Value::Object(converted))
}

fn responses_text_from_chat_format(response_format: &Value) -> Option<Value> {
    let object = response_format.as_object()?;
    let format_type = object.get("type")?.as_str()?;
    let format = if format_type == "json_schema" {
        let mut format = object.get("json_schema")?.as_object()?.clone();
        format.insert("type".to_string(), Value::String("json_schema".to_string()));
        Value::Object(format)
    } else {
        response_format.clone()
    };
    Some(json!({"format": format}))
}

/// Convert an OpenAI Chat Completions request into an upstream Responses request.
pub(super) fn openai_chat_request_to_responses(request: &Value, model: &str) -> Result<Value> {
    if let Some(native_request) = request
        .get(super::RESPONSES_REQUEST_PASSTHROUGH_KEY)
        .and_then(Value::as_object)
    {
        let mut body = Value::Object(native_request.clone());
        body["model"] = Value::String(model.to_string());
        body["stream"] = Value::Bool(false);
        if body.get("tool_choice").is_some_and(Value::is_null) {
            body.as_object_mut()
                .expect("native Responses request is an object")
                .remove("tool_choice");
        }
        return Ok(body);
    }

    let messages = request
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("OpenAI request requires a messages array"))?;
    let mut input = Vec::new();

    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .map(|role| if role == "system" { "developer" } else { role })
            .unwrap_or("user");
        if role == "tool" {
            input.push(json!({
                "type": "function_call_output",
                "call_id": message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("call_unknown"),
                "output": message.get("content").map(content_text).unwrap_or_default()
            }));
            continue;
        }

        let content = message
            .get("content")
            .map(|value| response_content_blocks(value, role))
            .transpose()?
            .unwrap_or_default();
        if !content.is_empty() {
            input.push(json!({"role": role, "content": content}));
        }

        if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
            for tool_call in tool_calls {
                let function = tool_call.get("function").unwrap_or(&Value::Null);
                input.push(json!({
                    "type": "function_call",
                    "call_id": tool_call
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("call_unknown"),
                    "name": function
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("function"),
                    "arguments": function_arguments(function.get("arguments"))
                }));
            }
        }
    }

    let mut body = json!({"model": model, "input": input, "stream": false});
    if let Some(max_tokens) = request
        .get("max_completion_tokens")
        .or_else(|| request.get("max_tokens"))
    {
        body["max_output_tokens"] = max_tokens.clone();
    }
    for key in ["temperature", "top_p", "parallel_tool_calls"] {
        if let Some(value) = request.get(key) {
            body[key] = value.clone();
        }
    }
    if let Some(text) = request
        .get("response_format")
        .and_then(responses_text_from_chat_format)
    {
        body["text"] = text;
    }
    if let Some(reasoning) = request.get("reasoning").filter(|value| !value.is_null()) {
        body["reasoning"] = reasoning.clone();
    } else if let Some(reasoning_effort) = request
        .get("reasoning_effort")
        .filter(|value| !value.is_null())
    {
        let reasoning_effort = reasoning_effort
            .as_str()
            .ok_or_else(|| anyhow!("OpenAI reasoning_effort must be a string"))?;
        body["reasoning"] = json!({"effort": reasoning_effort});
    }
    if let Some(previous_response_id) = request.get("previous_response_id") {
        body["previous_response_id"] = previous_response_id.clone();
    }
    if let Some(tools) = request.get("tools").and_then(Value::as_array) {
        body["tools"] = Value::Array(tools.iter().filter_map(responses_tool).collect());
    }
    if let Some(tool_choice) = request.get("tool_choice") {
        if let Some(tool_choice) = responses_tool_choice(tool_choice) {
            body["tool_choice"] = tool_choice;
        }
    }
    Ok(body)
}

fn response_text(response: &Value) -> String {
    let mut text = String::new();
    if let Some(output) = response.get("output").and_then(Value::as_array) {
        for item in output {
            if item.get("type").and_then(Value::as_str) != Some("message") {
                continue;
            }
            if let Some(content) = item.get("content").and_then(Value::as_array) {
                for part in content {
                    if matches!(
                        part.get("type").and_then(Value::as_str),
                        Some("output_text" | "text")
                    ) {
                        if let Some(value) = part.get("text").and_then(Value::as_str) {
                            text.push_str(value);
                        }
                    }
                }
            }
        }
    }
    if text.is_empty() {
        response
            .get("output_text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    } else {
        text
    }
}

fn response_refusal_text(response: &Value) -> String {
    let mut refusal = String::new();
    if let Some(output) = response.get("output").and_then(Value::as_array) {
        for item in output {
            if item.get("type").and_then(Value::as_str) == Some("refusal") {
                if let Some(value) = item
                    .get("refusal")
                    .or_else(|| item.get("text"))
                    .and_then(Value::as_str)
                {
                    refusal.push_str(value);
                }
                continue;
            }
            if item.get("type").and_then(Value::as_str) != Some("message") {
                continue;
            }
            if let Some(content) = item.get("content").and_then(Value::as_array) {
                for part in content {
                    if part.get("type").and_then(Value::as_str) == Some("refusal") {
                        if let Some(value) = part
                            .get("refusal")
                            .or_else(|| part.get("text"))
                            .and_then(Value::as_str)
                        {
                            refusal.push_str(value);
                        }
                    }
                }
            }
        }
    }
    refusal
}

fn response_reasoning_text(response: &Value) -> String {
    let mut reasoning = Vec::new();
    if let Some(output) = response.get("output").and_then(Value::as_array) {
        for item in output {
            if item.get("type").and_then(Value::as_str) != Some("reasoning") {
                continue;
            }
            for field in ["summary", "content"] {
                if let Some(parts) = item.get(field).and_then(Value::as_array) {
                    for part in parts {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            reasoning.push(text);
                        }
                    }
                }
            }
        }
    }
    reasoning.join("\n\n")
}

fn response_tool_calls(response: &Value) -> Vec<Value> {
    response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .map(|item| {
            let arguments = function_arguments(item.get("arguments"));
            json!({
                "id": item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("id").and_then(Value::as_str))
                    .unwrap_or("call_unknown"),
                "type": "function",
                "function": {
                    "name": item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("function"),
                    "arguments": arguments
                }
            })
        })
        .collect()
}

/// Convert a completed Responses API payload to OpenAI Chat Completions JSON.
pub(super) fn responses_response_to_openai_chat(response: &Value, model: &str) -> Result<Value> {
    if let Some(error) = response.get("error").filter(|error| !error.is_null()) {
        return Err(anyhow!("Responses provider returned an error: {error}"));
    }
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Responses provider payload is missing output"))?;
    let text = response_text(response);
    let refusal = response_refusal_text(response);
    let reasoning = response_reasoning_text(response);
    let tool_calls = response_tool_calls(response);
    if output.is_empty() && text.is_empty() && refusal.is_empty() && tool_calls.is_empty() {
        return Err(anyhow!("Responses provider returned no output items"));
    }

    let mut message = json!({
        "role": "assistant",
        "content": if text.is_empty() { Value::Null } else { Value::String(text) }
    });
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls.clone());
    }
    if !reasoning.is_empty() {
        message["reasoning_content"] = Value::String(reasoning);
    }
    if !refusal.is_empty() {
        message["refusal"] = Value::String(refusal);
    }
    let incomplete_reason = response
        .get("incomplete_details")
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str);
    let finish_reason = if !tool_calls.is_empty() {
        "tool_calls"
    } else if let Some(reason) = incomplete_reason {
        match reason {
            "max_output_tokens" => "length",
            "content_filter" => "content_filter",
            _ => "stop",
        }
    } else {
        "stop"
    };
    let usage = response.get("usage").cloned().unwrap_or_else(|| json!({}));
    let prompt_tokens = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let completion_tokens = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let mut openai_usage = json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(prompt_tokens.saturating_add(completion_tokens))
    });
    if let Some(details) = usage.get("input_tokens_details") {
        openai_usage["prompt_tokens_details"] = details.clone();
    }
    if let Some(details) = usage.get("output_tokens_details") {
        openai_usage["completion_tokens_details"] = details.clone();
    }

    let mut converted = json!({
        "id": response.get("id").and_then(Value::as_str).unwrap_or("resp_unknown"),
        "object": "chat.completion",
        "created": response_created_at(response.get("created_at")),
        "model": response.get("model").and_then(Value::as_str).unwrap_or(model),
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason
        }],
        "usage": openai_usage
    });
    if let Some(status) = response.get("status").and_then(Value::as_str) {
        converted["response_status"] = Value::String(status.to_string());
    }
    if let Some(incomplete_details) = response
        .get("incomplete_details")
        .filter(|value| !value.is_null())
    {
        converted["incomplete_details"] = incomplete_details.clone();
    }
    Ok(converted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_chat_request_to_meta_responses_shape() {
        let request = json!({
            "model": "meta-muse,muse-spark-1.1",
            "messages": [{"role": "user", "content": "Write a haiku"}],
            "max_completion_tokens": 64,
            "reasoning_effort": "high",
            "stream": true
        });

        let converted = openai_chat_request_to_responses(&request, "muse-spark-1.1").unwrap();

        assert_eq!(converted["model"], "muse-spark-1.1");
        assert_eq!(converted["stream"], false);
        assert_eq!(converted["max_output_tokens"], 64);
        assert_eq!(converted["reasoning"], json!({"effort": "high"}));
        assert_eq!(converted["input"][0]["role"], "user");
        assert_eq!(converted["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(converted["input"][0]["content"][0]["text"], "Write a haiku");
        assert!(converted.get("messages").is_none());
    }

    #[test]
    fn normalizes_system_role_and_supported_tools() {
        let request = json!({
            "messages": [
                {"role": "system", "content": "Be concise"},
                {"role": "user", "content": [
                    {"type": "image_url", "image_url": {}},
                    {"type": "image_url", "image_url": {"url": "https://example.test/a.png"}}
                ]},
                {"role": "assistant", "content": "", "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "flat", "arguments": ""}
                }]}
            ],
            "tools": [
                {"type": "function", "name": "flat", "parameters": {"type": "object"}},
                {"type": "function", "function": {"name": "nested", "parameters": {}}},
                {"type": "code_interpreter"}
            ],
            "tool_choice": {"type": "function", "function": {"name": "nested"}}
        });

        let converted = openai_chat_request_to_responses(&request, "muse").unwrap();

        assert_eq!(converted["input"][0]["role"], "developer");
        assert_eq!(
            converted["input"][1]["content"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            converted["input"][1]["content"][0]["image_url"],
            "https://example.test/a.png"
        );
        assert_eq!(converted["tools"].as_array().unwrap().len(), 3);
        assert_eq!(converted["tools"][0]["name"], "flat");
        assert_eq!(converted["tools"][1]["name"], "nested");
        assert_eq!(converted["tools"][2]["type"], "code_interpreter");
        assert_eq!(
            converted["tool_choice"],
            json!({"type": "function", "name": "nested"})
        );
        assert_eq!(converted["input"][2]["arguments"], "{}");
    }

    #[test]
    fn reasoning_null_falls_back_to_valid_effort() {
        let request = json!({
            "messages": [{"role": "user", "content": "Think"}],
            "reasoning": null,
            "reasoning_effort": "medium"
        });

        let converted = openai_chat_request_to_responses(&request, "muse").unwrap();

        assert_eq!(converted["reasoning"], json!({"effort": "medium"}));
    }

    #[test]
    fn maps_chat_json_schema_to_responses_text_format() {
        let request = json!({
            "messages": [{"role": "user", "content": "Return JSON"}],
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "answer",
                    "schema": {
                        "type": "object",
                        "properties": {"answer": {"type": "string"}},
                        "required": ["answer"],
                        "additionalProperties": false
                    },
                    "strict": true
                }
            }
        });

        let converted = openai_chat_request_to_responses(&request, "muse").unwrap();

        assert_eq!(
            converted["text"]["format"],
            json!({
                "type": "json_schema",
                "name": "answer",
                "schema": {
                    "type": "object",
                    "properties": {"answer": {"type": "string"}},
                    "required": ["answer"],
                    "additionalProperties": false
                },
                "strict": true
            })
        );
    }

    #[test]
    fn preserves_internal_responses_request() {
        let mut request = json!({
            "messages": [{"role": "user", "content": "Think"}],
            "reasoning_effort": "high"
        });
        request[super::super::RESPONSES_REQUEST_PASSTHROUGH_KEY] = json!({
            "model": "auto",
            "input": [{"role": "user", "content": [{
                "type": "input_file",
                "file_id": "file_123"
            }]}],
            "reasoning": {"effort": "high", "summary": "detailed"},
            "text": {"format": {"type": "json_schema", "name": "answer"}},
            "stream": true
        });

        let converted = openai_chat_request_to_responses(&request, "muse").unwrap();

        assert_eq!(converted["model"], "muse");
        assert_eq!(converted["stream"], false);
        assert_eq!(converted["input"][0]["content"][0]["type"], "input_file");
        assert_eq!(converted["input"][0]["content"][0]["file_id"], "file_123");
        assert_eq!(
            converted["reasoning"],
            json!({"effort": "high", "summary": "detailed"})
        );
        assert_eq!(converted["text"]["format"]["type"], "json_schema");
        assert!(converted
            .get(super::super::RESPONSES_REQUEST_PASSTHROUGH_KEY)
            .is_none());
    }

    #[test]
    fn drops_invalid_direct_chat_tools_and_choices() {
        let request = json!({
            "messages": [{"role": "user", "content": "Think"}],
            "tools": [null, 7, {}, {"type": ""}, {"type": "web_search"}],
            "tool_choice": null
        });

        let converted = openai_chat_request_to_responses(&request, "muse").unwrap();

        assert_eq!(converted["tools"], json!([{"type": "web_search"}]));
        assert!(converted.get("tool_choice").is_none());
    }

    #[test]
    fn rejects_non_string_reasoning_effort() {
        let request = json!({
            "messages": [{"role": "user", "content": "Think"}],
            "reasoning_effort": {"unexpected": true}
        });

        let error = openai_chat_request_to_responses(&request, "muse").unwrap_err();

        assert_eq!(
            error.to_string(),
            "OpenAI reasoning_effort must be a string"
        );
    }

    #[test]
    fn rejects_non_string_text_content_blocks() {
        let request = json!({
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": {"unexpected": true}}]
            }]
        });

        let error = openai_chat_request_to_responses(&request, "muse").unwrap_err();

        assert_eq!(
            error.to_string(),
            "Responses text content block requires string 'text'"
        );
    }

    #[test]
    fn converts_responses_text_and_usage_to_chat_shape() {
        let response = json!({
            "id": "resp_meta_1",
            "object": "response",
            "created_at": 42,
            "status": "completed",
            "error": null,
            "incomplete_details": null,
            "model": "muse-spark-1.1",
            "output": [
                {
                    "type": "reasoning",
                    "summary": [
                        {"type": "summary_text", "text": "Checked the route."},
                        {"type": "summary_text", "text": "Selected Muse."}
                    ]
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "AlphaHENG ready"}]
                }
            ],
            "usage": {
                "input_tokens": 11,
                "input_tokens_details": {"cached_tokens": 4},
                "output_tokens": 3,
                "output_tokens_details": {"reasoning_tokens": 2},
                "total_tokens": 14
            }
        });

        let converted = responses_response_to_openai_chat(&response, "muse-spark-1.1").unwrap();

        assert_eq!(
            converted["choices"][0]["message"]["content"],
            "AlphaHENG ready"
        );
        assert_eq!(converted["choices"][0]["finish_reason"], "stop");
        assert_eq!(
            converted["choices"][0]["message"]["reasoning_content"],
            "Checked the route.\n\nSelected Muse."
        );
        assert_eq!(converted["usage"]["prompt_tokens"], 11);
        assert_eq!(converted["usage"]["completion_tokens"], 3);
        assert_eq!(
            converted["usage"]["prompt_tokens_details"]["cached_tokens"],
            4
        );
        assert_eq!(
            converted["usage"]["completion_tokens_details"]["reasoning_tokens"],
            2
        );
    }

    #[test]
    fn preserves_function_calls_across_responses_protocol() {
        let response = json!({
            "id": "resp_meta_tool",
            "model": "muse-spark-1.1",
            "output": [{
                "type": "function_call",
                "call_id": null,
                "id": "call_7",
                "name": "run_check",
                "arguments": "   "
            }]
        });

        let converted = responses_response_to_openai_chat(&response, "muse-spark-1.1").unwrap();

        assert_eq!(converted["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            converted["choices"][0]["message"]["tool_calls"][0]["id"],
            "call_7"
        );
        assert_eq!(
            converted["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "run_check"
        );
        assert_eq!(
            converted["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{}"
        );
    }

    #[test]
    fn preserves_refusal_text_and_content_filter_reason() {
        let response = json!({
            "id": "resp_refusal",
            "created_at": "42",
            "status": "incomplete",
            "incomplete_details": {"reason": "content_filter"},
            "output": [{
                "type": "message",
                "content": [{"type": "refusal", "refusal": "I cannot help with that."}]
            }]
        });

        let converted = responses_response_to_openai_chat(&response, "muse").unwrap();

        assert_eq!(converted["created"], 42);
        assert_eq!(converted["choices"][0]["finish_reason"], "content_filter");
        assert_eq!(converted["response_status"], "incomplete");
        assert_eq!(
            converted["incomplete_details"],
            json!({"reason": "content_filter"})
        );
        assert!(converted["choices"][0]["message"]["content"].is_null());
        assert_eq!(
            converted["choices"][0]["message"]["refusal"],
            "I cannot help with that."
        );
    }

    #[test]
    fn parses_rfc3339_response_timestamp() {
        let response = json!({
            "id": "resp_timestamp",
            "created_at": "2026-08-01T12:34:56Z",
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "ok"}]
            }]
        });

        let converted = responses_response_to_openai_chat(&response, "muse").unwrap();

        assert_eq!(converted["created"], 1_785_587_696_i64);
    }
}
