// SPDX-License-Identifier: AGPL-3.0-or-later
// OpenAI Responses API compatibility layer.
//
// Converts between OpenAI Responses API format and OpenAI Chat Completions format.
// This handles the `/v1/responses` endpoint.

use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use tracing::error;

use super::{openai_compat::handle_chat_completions, AppState};

fn parse_sse_frames(payload: &str) -> Vec<(Option<String>, String)> {
    let mut frames = Vec::new();
    let normalized = payload.replace("\r\n", "\n");

    for frame in normalized.split("\n\n") {
        if frame.trim().is_empty() {
            continue;
        }

        let mut event_type = None;
        let mut data_lines: Vec<String> = Vec::new();
        for line in frame.lines() {
            if let Some(rest) = line.strip_prefix("event:") {
                event_type = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                data_lines.push(rest.trim_start().to_string());
            }
        }

        if data_lines.is_empty() {
            continue;
        }
        frames.push((event_type, data_lines.join("\n")));
    }

    frames
}

fn looks_like_sse_payload(payload: &str) -> bool {
    let trimmed = payload.trim_start();
    trimmed.starts_with("event:") || trimmed.starts_with("data:")
}

pub(super) fn decode_request_body(bytes: &[u8], headers: &HeaderMap) -> Result<Vec<u8>, String> {
    let content_encoding = headers
        .get(axum::http::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    if content_encoding.is_empty() || content_encoding == "identity" {
        return Ok(bytes.to_vec());
    }

    if content_encoding.contains("zstd") || content_encoding.contains("zst") {
        return zstd::stream::decode_all(std::io::Cursor::new(bytes))
            .map_err(|e| format!("Failed to decode zstd request body: {}", e));
    }

    Err(format!(
        "Unsupported content-encoding '{}'",
        content_encoding
    ))
}

pub(super) fn parse_json_payload(bytes: &[u8]) -> Result<serde_json::Value, String> {
    if let Ok(value) = serde_json::from_slice(bytes) {
        return Ok(value);
    }

    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("Request body was empty".to_string());
    }

    // Some clients may double-encode JSON as a string payload.
    if let Ok(serde_json::Value::String(inner)) = serde_json::from_str::<serde_json::Value>(trimmed)
    {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&inner) {
            return Ok(value);
        }
    }

    // Defensive fallback: if there is envelope noise, parse the first JSON object span.
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if end > start {
            let candidate = &trimmed[start..=end];
            if let Ok(value) = serde_json::from_str(candidate) {
                return Ok(value);
            }
        }
    }

    let preview: String = trimmed.chars().take(200).collect();
    Err(format!(
        "Failed to parse request body as JSON (preview: {:?})",
        preview
    ))
}

fn map_openai_usage_to_responses_usage(usage: &serde_json::Value) -> serde_json::Value {
    let prompt_tokens = usage
        .get("prompt_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion_tokens = usage
        .get("completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(prompt_tokens + completion_tokens);
    let cached_tokens = usage
        .get("prompt_tokens_details")
        .and_then(|v| v.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let reasoning_tokens = usage
        .get("completion_tokens_details")
        .and_then(|v| v.get("reasoning_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    serde_json::json!({
        "input_tokens": prompt_tokens,
        "input_tokens_details": {
            "cached_tokens": cached_tokens
        },
        "output_tokens": completion_tokens,
        "output_tokens_details": {
            "reasoning_tokens": reasoning_tokens
        },
        "total_tokens": total_tokens
    })
}

fn openai_chat_completion_to_responses_json(openai: &serde_json::Value) -> serde_json::Value {
    let response_id = openai
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("resp_unknown");
    let created_at = openai
        .get("created")
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64
        });
    let model = openai
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let mut output_items = Vec::new();
    if let Some(choice) = openai
        .get("choices")
        .and_then(|choices| choices.as_array())
        .and_then(|choices| choices.first())
    {
        if let Some(message) = choice.get("message") {
            let mut content_blocks = Vec::new();
            if let Some(content) = message.get("content") {
                match content {
                    serde_json::Value::String(text) if !text.is_empty() => {
                        content_blocks.push(serde_json::json!({
                            "type": "output_text",
                            "text": text
                        }));
                    }
                    serde_json::Value::Array(items) => {
                        for item in items {
                            if item.get("type").and_then(|v| v.as_str()) == Some("text") {
                                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                                    content_blocks.push(serde_json::json!({
                                        "type": "output_text",
                                        "text": text
                                    }));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }

            output_items.push(serde_json::json!({
                "id": format!("msg_{}", response_id),
                "type": "message",
                "role": "assistant",
                "content": content_blocks
            }));

            if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
                for tool_call in tool_calls {
                    let call_id = tool_call
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("call_unknown");
                    let name = tool_call
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool");
                    let arguments = tool_call
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("{}");

                    output_items.push(serde_json::json!({
                        "id": call_id,
                        "type": "function_call",
                        "call_id": call_id,
                        "name": name,
                        "arguments": arguments
                    }));
                }
            }
        }
    }

    let usage = openai
        .get("usage")
        .map(map_openai_usage_to_responses_usage)
        .unwrap_or_else(|| map_openai_usage_to_responses_usage(&serde_json::json!({})));

    serde_json::json!({
        "id": response_id,
        "object": "response",
        "created_at": created_at,
        "status": "completed",
        "model": model,
        "output": output_items,
        "usage": usage
    })
}

fn responses_content_to_openai_content(content: &serde_json::Value) -> serde_json::Value {
    match content {
        serde_json::Value::Array(items) => {
            let mut blocks = Vec::new();
            for item in items {
                let block_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match block_type {
                    "input_text" | "output_text" => {
                        if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                            blocks.push(serde_json::json!({"type": "text", "text": text}));
                        }
                    }
                    "input_image" => {
                        if let Some(image_url) = item.get("image_url").and_then(|v| v.as_str()) {
                            blocks.push(serde_json::json!({
                                "type": "image_url",
                                "image_url": {"url": image_url}
                            }));
                        }
                    }
                    _ => {
                        if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                            blocks.push(serde_json::json!({"type": "text", "text": text}));
                        }
                    }
                }
            }
            if blocks.len() == 1 && blocks[0].get("type").and_then(|v| v.as_str()) == Some("text") {
                serde_json::Value::String(
                    blocks[0]
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                )
            } else {
                serde_json::Value::Array(blocks)
            }
        }
        serde_json::Value::String(text) => serde_json::Value::String(text.clone()),
        _ => serde_json::Value::String(content.to_string()),
    }
}

fn normalize_tool_output(output: &serde_json::Value) -> String {
    match output {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => {
            let mut combined = String::new();
            for item in items {
                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                    combined.push_str(text);
                }
            }
            if combined.is_empty() {
                output.to_string()
            } else {
                combined
            }
        }
        _ => output.to_string(),
    }
}

fn normalize_responses_message_role(role: &str) -> &str {
    match role {
        // OpenAI Responses API `developer` role should be treated as `system`
        // for providers that only accept classic OpenAI chat roles.
        "developer" => "system",
        _ => role,
    }
}

pub(super) fn responses_request_to_openai_chat_request(
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "responses request requires 'model'".to_string())?;

    let mut messages: Vec<serde_json::Value> = Vec::new();

    if let Some(instructions) = body.get("instructions").and_then(|v| v.as_str()) {
        if !instructions.is_empty() {
            messages.push(serde_json::json!({
                "role": "system",
                "content": instructions
            }));
        }
    }

    if let Some(input_items) = body.get("input").and_then(|v| v.as_array()) {
        for item in input_items {
            let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match item_type {
                "message" => {
                    let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("user");
                    let role = normalize_responses_message_role(role);
                    let content = item
                        .get("content")
                        .map(responses_content_to_openai_content)
                        .unwrap_or_else(|| serde_json::Value::String(String::new()));
                    messages.push(serde_json::json!({
                        "role": role,
                        "content": content
                    }));
                }
                "function_call_output" | "custom_tool_call_output" => {
                    let call_id = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("call_unknown");
                    let output = item
                        .get("output")
                        .map(normalize_tool_output)
                        .unwrap_or_default();
                    messages.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": output
                    }));
                }
                "function_call" | "custom_tool_call" | "local_shell_call" => {
                    let call_id = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("id").and_then(|v| v.as_str()))
                        .unwrap_or("call_unknown");
                    let name = match item_type {
                        "function_call" => item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("function_call"),
                        "custom_tool_call" => item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("custom_tool_call"),
                        _ => "local_shell",
                    };

                    let arguments = match item_type {
                        "function_call" => item
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| {
                                item.get("arguments")
                                    .cloned()
                                    .unwrap_or_else(|| serde_json::json!({}))
                                    .to_string()
                            }),
                        "custom_tool_call" => item
                            .get("input")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| {
                                item.get("input")
                                    .cloned()
                                    .unwrap_or_else(|| serde_json::json!({}))
                                    .to_string()
                            }),
                        _ => item
                            .get("action")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({}))
                            .to_string(),
                    };

                    messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": "",
                        "tool_calls": [{
                            "id": call_id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": arguments
                            }
                        }]
                    }));
                }
                _ => {}
            }
        }
    }

    let mut request = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": body.get("stream").and_then(|v| v.as_bool()).unwrap_or(true)
    });

    if let Some(tools) = body.get("tools").cloned() {
        request["tools"] = tools;
    }
    if let Some(tool_choice) = body.get("tool_choice").cloned() {
        request["tool_choice"] = tool_choice;
    }
    if let Some(temperature) = body.get("temperature").cloned() {
        request["temperature"] = temperature;
    }
    if let Some(top_p) = body.get("top_p").cloned() {
        request["top_p"] = top_p;
    }
    if let Some(max_tokens) = body.get("max_output_tokens").cloned() {
        // The OpenAI Responses API names this field `max_output_tokens`,
        // while newer OpenAI chat/reasoning models reject legacy `max_tokens`.
        // The downstream Anthropic translation path also understands
        // `max_completion_tokens`, so this preserves Codex compatibility
        // without breaking Anthropic-protocol providers.
        request["max_completion_tokens"] = max_tokens;
    }

    Ok(request)
}

async fn convert_openai_json_response_to_responses(response: Response) -> Response {
    let (mut parts, body) = response.into_parts();
    let body_bytes = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(err) => {
            error!("Failed to read OpenAI response: {}", err);
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": "Failed to read upstream response"})),
            )
                .into_response();
        }
    };

    let openai_json: serde_json::Value = match serde_json::from_slice(&body_bytes) {
        Ok(value) => value,
        Err(_) => {
            // Defensive fallback: some upstream paths can still return SSE here
            // (e.g. force-non-streaming mismatches). Convert it instead of
            // passing through Anthropic/OpenAI event payloads to Responses clients.
            let payload = String::from_utf8_lossy(&body_bytes);
            if looks_like_sse_payload(&payload) {
                parts.headers.insert(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("text/event-stream"),
                );
                parts.status = StatusCode::OK;
                return Response::from_parts(
                    parts,
                    Body::from(convert_sse_payload_to_responses(&payload)),
                );
            }
            return Response::from_parts(parts, Body::from(body_bytes));
        }
    };

    if parts.status != StatusCode::OK {
        // Check if this is a rate limit error (429)
        let is_rate_limit = parts.status == StatusCode::TOO_MANY_REQUESTS;
        let retry_after = parts
            .headers
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());

        let error_message = openai_json
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("upstream request failed");

        let mut error_obj = serde_json::json!({
            "message": error_message
        });

        if is_rate_limit {
            error_obj["code"] = serde_json::Value::String("rate_limited".to_string());
            if let Some(retry_after_secs) = retry_after {
                error_obj["retry_after"] = serde_json::json!(retry_after_secs);
            }
        }

        let failed = serde_json::json!({
            "id": "resp_failed",
            "object": "response",
            "status": "failed",
            "error": error_obj
        });

        // Preserve the original status code and retry-after header for rate limits
        if is_rate_limit {
            if let Some(retry_after_secs) = retry_after {
                parts.headers.insert(
                    "retry-after",
                    axum::http::HeaderValue::from_str(&retry_after_secs.to_string())
                        .unwrap_or(axum::http::HeaderValue::from_static("0")),
                );
            }
        }

        parts.headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        return Response::from_parts(parts, Body::from(failed.to_string()));
    }

    let responses_json = openai_chat_completion_to_responses_json(&openai_json);
    parts.headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    Response::from_parts(parts, Body::from(responses_json.to_string()))
}

async fn convert_openai_stream_response_to_responses(response: Response) -> Response {
    let (mut parts, body) = response.into_parts();
    let body_bytes = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(err) => {
            error!("Failed to read OpenAI stream: {}", err);
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": "Failed to read upstream stream"})),
            )
                .into_response();
        }
    };

    let payload = String::from_utf8_lossy(&body_bytes);

    if parts.status != StatusCode::OK {
        let mut output = String::new();
        // Check if this is a rate limit error (429)
        let is_rate_limit = parts.status == StatusCode::TOO_MANY_REQUESTS;
        let retry_after = parts
            .headers
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());

        let error_text = String::from_utf8_lossy(&body_bytes).to_string();

        let mut error_obj = serde_json::json!({
            "message": error_text
        });

        if is_rate_limit {
            error_obj["code"] = serde_json::Value::String("rate_limited".to_string());
            if let Some(retry_after_secs) = retry_after {
                error_obj["retry_after"] = serde_json::json!(retry_after_secs);
            }
        } else {
            error_obj["code"] = serde_json::Value::String("upstream_error".to_string());
        }

        let failed_event = serde_json::json!({
            "type": "response.failed",
            "response": {
                "id": "resp_failed",
                "object": "response",
                "status": "failed",
                "error": error_obj
            }
        });
        output.push_str("event: response.failed\ndata: ");
        output.push_str(&failed_event.to_string());
        output.push_str("\n\n");
        parts.status = StatusCode::OK;
        parts.headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("text/event-stream"),
        );
        return Response::from_parts(parts, Body::from(output));
    }

    let output = convert_sse_payload_to_responses(&payload);

    parts.headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/event-stream"),
    );
    parts.status = StatusCode::OK;
    Response::from_parts(parts, Body::from(output))
}

fn convert_sse_payload_to_responses(payload: &str) -> String {
    #[derive(Default)]
    struct ToolAccum {
        id: String,
        name: String,
        arguments: String,
        added: bool,
    }

    let mut output = String::new();
    let mut response_id = "resp_stream".to_string();
    let mut created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let mut model = "unknown".to_string();
    let mut created_sent = false;
    let mut message_item_added = false;
    let mut message_text = String::new();
    let mut reasoning_text = String::new();
    let mut tools: std::collections::BTreeMap<usize, ToolAccum> = std::collections::BTreeMap::new();
    let mut usage = map_openai_usage_to_responses_usage(&serde_json::json!({}));

    for (event_type, data) in parse_sse_frames(payload) {
        if data.trim() == "[DONE]" {
            break;
        }

        let chunk: serde_json::Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // OpenAI chunk metadata
        if let Some(id) = chunk.get("id").and_then(|v| v.as_str()) {
            response_id = id.to_string();
        }
        if let Some(ts) = chunk.get("created").and_then(|v| v.as_i64()) {
            created_at = ts;
        }
        if let Some(m) = chunk.get("model").and_then(|v| v.as_str()) {
            model = m.to_string();
        }

        // Anthropic message_start metadata fallback
        if event_type.as_deref() == Some("message_start")
            || chunk.get("type").and_then(|v| v.as_str()) == Some("message_start")
        {
            if let Some(msg) = chunk.get("message") {
                if let Some(id) = msg.get("id").and_then(|v| v.as_str()) {
                    response_id = id.to_string();
                }
                if let Some(m) = msg.get("model").and_then(|v| v.as_str()) {
                    model = m.to_string();
                }
                if let Some(u) = msg.get("usage") {
                    usage = map_openai_usage_to_responses_usage(&serde_json::json!({
                        "prompt_tokens": u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                        "completion_tokens": u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                        "total_tokens": u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0)
                            + u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0)
                    }));
                }
            }
        }

        if !created_sent {
            let created_event = serde_json::json!({
                "type": "response.created",
                "response": {
                    "id": response_id,
                    "object": "response",
                    "created_at": created_at,
                    "status": "in_progress",
                    "model": model
                }
            });
            output.push_str("event: response.created\ndata: ");
            output.push_str(&created_event.to_string());
            output.push_str("\n\n");
            created_sent = true;
        }

        if let Some(u) = chunk.get("usage") {
            usage = map_openai_usage_to_responses_usage(u);
        }

        // OpenAI chunk path
        let choice = chunk
            .get("choices")
            .and_then(|v| v.as_array())
            .and_then(|v| v.first());

        if let Some(choice) = choice {
            let delta = choice.get("delta").cloned().unwrap_or_default();

            if delta
                .get("role")
                .and_then(|v| v.as_str())
                .is_some_and(|r| r == "assistant")
                && !message_item_added
            {
                let added = serde_json::json!({
                    "type": "response.output_item.added",
                    "item": {
                        "id": format!("msg_{}", response_id),
                        "type": "message",
                        "role": "assistant",
                        "content": []
                    }
                });
                output.push_str("event: response.output_item.added\ndata: ");
                output.push_str(&added.to_string());
                output.push_str("\n\n");
                message_item_added = true;
            }

            if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                if !message_item_added {
                    let added = serde_json::json!({
                        "type": "response.output_item.added",
                        "item": {
                            "id": format!("msg_{}", response_id),
                            "type": "message",
                            "role": "assistant",
                            "content": []
                        }
                    });
                    output.push_str("event: response.output_item.added\ndata: ");
                    output.push_str(&added.to_string());
                    output.push_str("\n\n");
                    message_item_added = true;
                }

                message_text.push_str(text);
                let delta_event = serde_json::json!({
                    "type": "response.output_text.delta",
                    "delta": text
                });
                output.push_str("event: response.output_text.delta\ndata: ");
                output.push_str(&delta_event.to_string());
                output.push_str("\n\n");
            }

            if let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                reasoning_text.push_str(reasoning);
                let delta_event = serde_json::json!({
                    "type": "response.reasoning_text.delta",
                    "delta": reasoning,
                    "content_index": 0
                });
                output.push_str("event: response.reasoning_text.delta\ndata: ");
                output.push_str(&delta_event.to_string());
                output.push_str("\n\n");
            }

            if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for tool_call in tool_calls {
                    let index =
                        tool_call.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let entry = tools.entry(index).or_default();

                    if let Some(id) = tool_call.get("id").and_then(|v| v.as_str()) {
                        entry.id = id.to_string();
                    }
                    if let Some(name) = tool_call
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())
                    {
                        entry.name = name.to_string();
                    }
                    if let Some(args) = tool_call
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|v| v.as_str())
                    {
                        entry.arguments.push_str(args);
                    }

                    if !entry.added && (!entry.id.is_empty() || !entry.name.is_empty()) {
                        if entry.id.is_empty() {
                            entry.id = format!("call_{}", index);
                        }
                        if entry.name.is_empty() {
                            entry.name = "tool".to_string();
                        }
                        let added = serde_json::json!({
                            "type": "response.output_item.added",
                            "item": {
                                "id": entry.id,
                                "type": "function_call",
                                "call_id": entry.id,
                                "name": entry.name,
                                "arguments": entry.arguments
                            }
                        });
                        output.push_str("event: response.output_item.added\ndata: ");
                        output.push_str(&added.to_string());
                        output.push_str("\n\n");
                        entry.added = true;
                    }
                }
            }
            continue;
        }

        // Anthropic event fallback path (when OpenAI conversion did not happen upstream)
        let event_type = event_type.or_else(|| {
            chunk
                .get("type")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });
        match event_type.as_deref() {
            Some("content_block_delta") => {
                if !message_item_added {
                    let added = serde_json::json!({
                        "type": "response.output_item.added",
                        "item": {
                            "id": format!("msg_{}", response_id),
                            "type": "message",
                            "role": "assistant",
                            "content": []
                        }
                    });
                    output.push_str("event: response.output_item.added\ndata: ");
                    output.push_str(&added.to_string());
                    output.push_str("\n\n");
                    message_item_added = true;
                }

                if let Some(delta) = chunk.get("delta") {
                    if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            message_text.push_str(text);
                            let delta_event = serde_json::json!({
                                "type": "response.output_text.delta",
                                "delta": text
                            });
                            output.push_str("event: response.output_text.delta\ndata: ");
                            output.push_str(&delta_event.to_string());
                            output.push_str("\n\n");
                        }
                    }
                    if let Some(thinking) = delta.get("thinking").and_then(|v| v.as_str()) {
                        if !thinking.is_empty() {
                            reasoning_text.push_str(thinking);
                            let delta_event = serde_json::json!({
                                "type": "response.reasoning_text.delta",
                                "delta": thinking,
                                "content_index": 0
                            });
                            output.push_str("event: response.reasoning_text.delta\ndata: ");
                            output.push_str(&delta_event.to_string());
                            output.push_str("\n\n");
                        }
                    }
                }
            }
            Some("message_delta") => {
                if let Some(u) = chunk.get("usage") {
                    usage = map_openai_usage_to_responses_usage(&serde_json::json!({
                        "prompt_tokens": u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                        "completion_tokens": u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                        "total_tokens": u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0)
                            + u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0)
                    }));
                }
            }
            _ => {}
        }
    }

    let mut output_items = Vec::new();

    if message_item_added {
        let mut message_content = Vec::new();
        if !reasoning_text.is_empty() {
            message_content.push(serde_json::json!({
                "type": "output_text",
                "text": reasoning_text
            }));
        }
        if !message_text.is_empty() {
            message_content.push(serde_json::json!({
                "type": "output_text",
                "text": message_text
            }));
        }
        let message_item = serde_json::json!({
            "id": format!("msg_{}", response_id),
            "type": "message",
            "role": "assistant",
            "content": message_content
        });
        output_items.push(message_item.clone());
        let done = serde_json::json!({
            "type": "response.output_item.done",
            "item": message_item
        });
        output.push_str("event: response.output_item.done\ndata: ");
        output.push_str(&done.to_string());
        output.push_str("\n\n");
    }

    for tool in tools.values() {
        let call_id = if tool.id.is_empty() {
            "call_unknown"
        } else {
            &tool.id
        };
        let name = if tool.name.is_empty() {
            "tool"
        } else {
            &tool.name
        };
        let item = serde_json::json!({
            "id": call_id,
            "type": "function_call",
            "call_id": call_id,
            "name": name,
            "arguments": tool.arguments
        });
        output_items.push(item.clone());
        let done = serde_json::json!({
            "type": "response.output_item.done",
            "item": item
        });
        output.push_str("event: response.output_item.done\ndata: ");
        output.push_str(&done.to_string());
        output.push_str("\n\n");
    }

    let completed = serde_json::json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "object": "response",
            "created_at": created_at,
            "status": "completed",
            "model": model,
            "output": output_items,
            "usage": usage
        }
    });
    output.push_str("event: response.completed\ndata: ");
    output.push_str(&completed.to_string());
    output.push_str("\n\n");

    output
}

/// Handle OpenAI Responses API requests.
pub async fn handle_responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let body_bytes = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": {
                        "message": format!("Failed to read request body: {}", err)
                    }
                })),
            )
                .into_response();
        }
    };

    let decoded = match decode_request_body(&body_bytes, &headers) {
        Ok(bytes) => bytes,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": {
                        "message": err
                    }
                })),
            )
                .into_response();
        }
    };

    let request_body = match parse_json_payload(&decoded) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": {
                        "message": err
                    }
                })),
            )
                .into_response();
        }
    };

    let stream_requested = request_body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let openai_chat_request = match responses_request_to_openai_chat_request(&request_body) {
        Ok(request) => request,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": {
                        "message": err
                    }
                })),
            )
                .into_response();
        }
    };

    let openai_response =
        handle_chat_completions(State(state), headers, Json(openai_chat_request)).await;

    if stream_requested {
        convert_openai_stream_response_to_responses(openai_response).await
    } else {
        convert_openai_json_response_to_responses(openai_response).await
    }
}
