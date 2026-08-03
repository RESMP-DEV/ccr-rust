// SPDX-License-Identifier: AGPL-3.0-or-later
// OpenAI Responses API compatibility layer.
//
// Converts between OpenAI Responses API format and OpenAI Chat Completions format.
// This handles the `/v1/responses` endpoint.

use axum::{
    body::{to_bytes, Body},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use futures::StreamExt;
use std::io::Read;
use tokio_stream::wrappers::ReceiverStream;
use tracing::error;

use crate::sse::SseFrameDecoder;

mod handler;
mod incremental_stream;

pub use handler::handle_responses;
use incremental_stream::ResponsesStreamConverter;

pub(super) const MAX_RESPONSES_BODY_BYTES: usize = 10 * 1024 * 1024;
const MAX_RESPONSES_ZSTD_WINDOW_LOG: u32 = 24;

#[derive(Debug)]
pub(super) enum DecodeRequestBodyError {
    PayloadTooLarge(String),
    UnsupportedEncoding(String),
    Invalid(String),
}

impl DecodeRequestBodyError {
    #[cfg(test)]
    fn contains(&self, needle: &str) -> bool {
        self.to_string().contains(needle)
    }

    pub(super) fn is_payload_too_large(&self) -> bool {
        matches!(self, Self::PayloadTooLarge(_))
    }
}

impl std::fmt::Display for DecodeRequestBodyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PayloadTooLarge(message)
            | Self::UnsupportedEncoding(message)
            | Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for DecodeRequestBodyError {}

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

pub(super) fn decode_request_body(
    bytes: &[u8],
    headers: &HeaderMap,
) -> Result<Vec<u8>, DecodeRequestBodyError> {
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
        let mut decoder =
            zstd::stream::read::Decoder::new(std::io::Cursor::new(bytes)).map_err(|e| {
                DecodeRequestBodyError::Invalid(format!(
                    "Failed to decode zstd request body: {}",
                    e
                ))
            })?;
        decoder
            .window_log_max(MAX_RESPONSES_ZSTD_WINDOW_LOG)
            .map_err(|e| {
                DecodeRequestBodyError::Invalid(format!(
                    "Failed to bound zstd request window: {}",
                    e
                ))
            })?;
        let mut decoded = Vec::new();
        decoder
            .take((MAX_RESPONSES_BODY_BYTES + 1) as u64)
            .read_to_end(&mut decoded)
            .map_err(|e| {
                DecodeRequestBodyError::Invalid(format!(
                    "Failed to decode zstd request body: {}",
                    e
                ))
            })?;
        if decoded.len() > MAX_RESPONSES_BODY_BYTES {
            return Err(DecodeRequestBodyError::PayloadTooLarge(format!(
                "Decoded request body exceeds {} bytes",
                MAX_RESPONSES_BODY_BYTES
            )));
        }
        return Ok(decoded);
    }

    Err(DecodeRequestBodyError::UnsupportedEncoding(format!(
        "Unsupported content-encoding '{}'",
        content_encoding
    )))
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

fn responses_reasoning_item(response_id: &str, reasoning: &str) -> serde_json::Value {
    serde_json::json!({
        "id": format!("rs_{}", response_id),
        "type": "reasoning",
        "summary": [{
            "type": "summary_text",
            "text": reasoning
        }]
    })
}

fn openai_chat_completion_to_responses_json(
    openai: &serde_json::Value,
    preserved_response: Option<&serde_json::Value>,
) -> serde_json::Value {
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

    if let Some(preserved_response) = preserved_response.filter(|response| response.is_object()) {
        return preserved_response.clone();
    }
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

            if let Some(reasoning) = message
                .get("reasoning_content")
                .and_then(|v| v.as_str())
                .filter(|reasoning| !reasoning.is_empty())
            {
                output_items.push(responses_reasoning_item(response_id, reasoning));
            }

            if let Some(refusal) = message
                .get("refusal")
                .and_then(|v| v.as_str())
                .filter(|refusal| !refusal.is_empty())
            {
                content_blocks.push(serde_json::json!({
                    "type": "refusal",
                    "refusal": refusal
                }));
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

    let mut response = serde_json::json!({
        "id": response_id,
        "object": "response",
        "created_at": created_at,
        "status": openai
            .get("response_status")
            .and_then(|value| value.as_str())
            .unwrap_or("completed"),
        "model": model,
        "output": output_items,
        "usage": usage
    });
    if let Some(incomplete_details) = openai
        .get("incomplete_details")
        .filter(|value| !value.is_null())
    {
        response["incomplete_details"] = incomplete_details.clone();
    }
    response
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

fn normalize_continuation_output(item: &serde_json::Value) -> String {
    if let Some(output) = item.get("output") {
        return normalize_tool_output(output);
    }
    let mut payload = item.as_object().cloned().unwrap_or_default();
    for identity_field in ["type", "call_id", "id", "approval_request_id"] {
        payload.remove(identity_field);
    }
    serde_json::Value::Object(payload).to_string()
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

    match body.get("input") {
        None => {}
        Some(input) if input.is_string() || input.is_array() => {}
        Some(_) => {
            return Err("responses request 'input' must be text or an array".to_string());
        }
    }

    if let Some(tools) = body.get("tools") {
        let tools = tools
            .as_array()
            .ok_or_else(|| "responses request 'tools' must be an array".to_string())?;
        for tool in tools {
            let tool_type = tool
                .as_object()
                .and_then(|object| object.get("type"))
                .and_then(|value| value.as_str())
                .filter(|tool_type| !tool_type.is_empty())
                .ok_or_else(|| {
                    "responses request tool entries require a non-empty string 'type'".to_string()
                })?;
            if tool_type == "function"
                && tool
                    .get("name")
                    .and_then(|value| value.as_str())
                    .filter(|name| !name.is_empty())
                    .is_none()
            {
                return Err(
                    "responses function tools require a non-empty string 'name'".to_string()
                );
            }
        }
    }
    if let Some(tool_choice) = body.get("tool_choice").filter(|value| !value.is_null()) {
        match tool_choice {
            serde_json::Value::String(choice) if !choice.is_empty() => {}
            serde_json::Value::Object(object) => {
                let choice_type = object
                    .get("type")
                    .and_then(|value| value.as_str())
                    .filter(|choice_type| !choice_type.is_empty())
                    .ok_or_else(|| {
                        "responses request 'tool_choice' must be a non-empty string or typed object"
                            .to_string()
                    })?;
                if choice_type == "function"
                    && object
                        .get("name")
                        .and_then(|value| value.as_str())
                        .filter(|name| !name.is_empty())
                        .is_none()
                {
                    return Err(
                        "responses function tool choices require a non-empty string 'name'"
                            .to_string(),
                    );
                }
            }
            _ => {
                return Err(
                    "responses request 'tool_choice' must be a non-empty string or typed object"
                        .to_string(),
                );
            }
        }
    }

    let mut messages: Vec<serde_json::Value> = Vec::new();

    if let Some(instructions) = body.get("instructions").and_then(|v| v.as_str()) {
        if !instructions.is_empty() {
            messages.push(serde_json::json!({
                "role": "system",
                "content": instructions
            }));
        }
    }

    if let Some(input) = body.get("input").and_then(|value| value.as_str()) {
        messages.push(serde_json::json!({
            "role": "user",
            "content": input
        }));
    }

    if let Some(input_items) = body.get("input").and_then(|value| value.as_array()) {
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
                "function_call_output"
                | "custom_tool_call_output"
                | "computer_call_output"
                | "local_shell_call_output"
                | "shell_call_output"
                | "apply_patch_call_output"
                | "mcp_approval_response" => {
                    let call_id = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("id").and_then(|v| v.as_str()))
                        .or_else(|| item.get("approval_request_id").and_then(|v| v.as_str()))
                        .unwrap_or("call_unknown");
                    let output = normalize_continuation_output(item);
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
    if messages.is_empty() {
        return Err("responses request requires 'input' or 'instructions'".to_string());
    }

    let mut request = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": body.get("stream").and_then(|v| v.as_bool()).unwrap_or(true)
    });

    if let Some(tools) = body.get("tools").cloned() {
        request["tools"] = tools;
    }
    if let Some(tool_choice) = body
        .get("tool_choice")
        .filter(|value| !value.is_null())
        .cloned()
    {
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
    if let Some(reasoning) = body.get("reasoning").filter(|value| !value.is_null()) {
        let reasoning = reasoning
            .as_object()
            .ok_or_else(|| "responses request 'reasoning' must be an object".to_string())?;
        let effort = match reasoning.get("effort").filter(|value| !value.is_null()) {
            Some(effort) => effort.as_str().ok_or_else(|| {
                "responses request 'reasoning.effort' must be a string".to_string()
            })?,
            None => "medium",
        };
        request["reasoning_effort"] = serde_json::Value::String(effort.to_string());
    }
    if let Some(previous_response_id) = body
        .get("previous_response_id")
        .filter(|value| !value.is_null())
        .cloned()
    {
        request["previous_response_id"] = previous_response_id;
    }
    Ok(request)
}

async fn convert_openai_json_response_to_responses(response: Response) -> Response {
    let (mut parts, body) = response.into_parts();
    let preserved_response = parts
        .extensions
        .get::<super::TrustedResponsesResponse>()
        .map(|response| response.0.clone());
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
                    Body::from(convert_sse_payload_to_responses(
                        &payload,
                        preserved_response.as_ref(),
                    )),
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

    let responses_json =
        openai_chat_completion_to_responses_json(&openai_json, preserved_response.as_ref());
    parts.headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    Response::from_parts(parts, Body::from(responses_json.to_string()))
}

async fn convert_openai_stream_response_to_responses(response: Response) -> Response {
    let (mut parts, body) = response.into_parts();
    let preserved_response = parts
        .extensions
        .get::<super::TrustedResponsesResponse>()
        .map(|response| response.0.clone());

    if parts.status != StatusCode::OK {
        // Exhausted-tier rate limits are an HTTP contract, not a successful SSE
        // stream. Preserve the synthesized status, headers, and structured body
        // so Responses clients can apply their normal retry policy.
        if parts.status == StatusCode::TOO_MANY_REQUESTS {
            return Response::from_parts(parts, body);
        }

        let body_bytes = match to_bytes(body, MAX_RESPONSES_BODY_BYTES).await {
            Ok(bytes) => bytes,
            Err(err) => {
                error!("Failed to read OpenAI stream error body: {}", err);
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(serde_json::json!({"error": "Failed to read upstream stream"})),
                )
                    .into_response();
            }
        };
        let mut output = String::new();
        let error_text = String::from_utf8_lossy(&body_bytes).to_string();
        let failed_event = serde_json::json!({
            "type": "response.failed",
            "response": {
                "id": "resp_failed",
                "object": "response",
                "status": "failed",
                "error": {
                    "message": error_text,
                    "code": "upstream_error"
                }
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

    if let Some(content_encoding) = parts.headers.get(axum::http::header::CONTENT_ENCODING) {
        let is_identity = content_encoding
            .to_str()
            .is_ok_and(|value| value.eq_ignore_ascii_case("identity"));
        if !is_identity {
            error!(
                encoding = ?content_encoding,
                "Responses stream adapter cannot translate an encoded upstream body"
            );
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": "Unsupported upstream stream content encoding"
                })),
            )
                .into_response();
        }
    }

    parts.headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/event-stream"),
    );
    parts.headers.insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache"),
    );
    parts.headers.remove(axum::http::header::CONTENT_LENGTH);
    parts.headers.remove(axum::http::header::CONTENT_ENCODING);
    parts.headers.remove(axum::http::header::TRANSFER_ENCODING);
    parts.headers.remove(axum::http::header::ETAG);
    parts.headers.remove(axum::http::header::LAST_MODIFIED);
    parts.status = StatusCode::OK;

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(100);
    tokio::spawn(async move {
        let mut stream = body.into_data_stream();
        let mut decoder = SseFrameDecoder::new();
        let mut converter = ResponsesStreamConverter::new(preserved_response);
        let mut received_bytes = 0_usize;

        while let Some(chunk) = stream.next().await {
            let bytes = match chunk {
                Ok(bytes) => bytes,
                Err(err) => {
                    error!("Failed to read OpenAI stream chunk: {}", err);
                    let failed = responses_stream_adapter_failed_event(
                        converter.response_id(),
                        "Failed to read upstream stream",
                    );
                    let _ = tx.send(Ok(Bytes::from(failed))).await;
                    return;
                }
            };
            received_bytes = received_bytes.saturating_add(bytes.len());
            if received_bytes > MAX_RESPONSES_BODY_BYTES {
                let failed = responses_stream_adapter_failed_event(
                    converter.response_id(),
                    "Upstream stream exceeded the Responses adapter limit",
                );
                let _ = tx.send(Ok(Bytes::from(failed))).await;
                return;
            }

            for frame in decoder.push(&bytes) {
                let converted = if frame.data.trim() == "[DONE]" {
                    converter.finish()
                } else {
                    converter.push_frame(frame.event.as_deref(), &frame.data)
                };
                if !converted.is_empty() && tx.send(Ok(Bytes::from(converted))).await.is_err() {
                    return;
                }
                if frame.data.trim() == "[DONE]" {
                    return;
                }
            }
        }

        let tail = converter.finish();
        if !tail.is_empty() {
            let _ = tx.send(Ok(Bytes::from(tail))).await;
        }
    });

    Response::from_parts(parts, Body::from_stream(ReceiverStream::new(rx)))
}

fn responses_stream_adapter_failed_event(response_id: &str, message: &str) -> String {
    let failed = serde_json::json!({
        "type": "response.failed",
        "response": {
            "id": response_id,
            "object": "response",
            "status": "failed",
            "error": {
                "message": message,
                "code": "stream_adapter_error"
            }
        }
    });
    format!("event: response.failed\ndata: {failed}\n\n")
}

fn initial_responses_output_item(item: &serde_json::Value) -> serde_json::Value {
    let mut initial = item.clone();
    match item.get("type").and_then(|value| value.as_str()) {
        Some("message") => initial["content"] = serde_json::json!([]),
        Some("reasoning") => {
            initial["summary"] = serde_json::json!([]);
            if initial.get("content").is_some() {
                initial["content"] = serde_json::json!([]);
            }
        }
        Some("function_call") => initial["arguments"] = serde_json::json!(""),
        _ => {}
    }
    initial
}

#[derive(Clone)]
struct ResponseOutputItemIdentity {
    output_index: usize,
    item_id: Option<String>,
    content_index: Option<usize>,
}

fn response_output_item_identity(
    response: &serde_json::Value,
    item_type: &str,
    occurrence: usize,
    content_type: Option<&str>,
) -> Option<ResponseOutputItemIdentity> {
    let (output_index, item) = response
        .get("output")?
        .as_array()?
        .iter()
        .enumerate()
        .filter(|(_, item)| item.get("type").and_then(|value| value.as_str()) == Some(item_type))
        .nth(occurrence)?;
    let content_index = content_type.map(|content_type| {
        item.get("content")
            .and_then(|content| content.as_array())
            .and_then(|content| {
                content.iter().position(|part| {
                    part.get("type").and_then(|value| value.as_str()) == Some(content_type)
                })
            })
            .unwrap_or(0)
    });
    Some(ResponseOutputItemIdentity {
        output_index,
        item_id: item
            .get("id")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        content_index,
    })
}

fn unique_response_output_item_identity(
    response: &serde_json::Value,
    item_type: &str,
    content_type: Option<&str>,
) -> Option<ResponseOutputItemIdentity> {
    let mut matching_items = response
        .get("output")?
        .as_array()?
        .iter()
        .enumerate()
        .filter(|(_, item)| item.get("type").and_then(|value| value.as_str()) == Some(item_type));
    let (output_index, item) = matching_items.next()?;
    if matching_items.next().is_some() {
        return None;
    }
    let content_index = if let Some(content_type) = content_type {
        let mut matching_content = item
            .get("content")
            .and_then(|content| content.as_array())
            .into_iter()
            .flatten()
            .enumerate()
            .filter(|(_, part)| {
                part.get("type").and_then(|value| value.as_str()) == Some(content_type)
            });
        let first = matching_content.next().map(|(index, _)| index)?;
        if matching_content.next().is_some() {
            return None;
        }
        Some(first)
    } else {
        None
    };
    Some(ResponseOutputItemIdentity {
        output_index,
        item_id: item
            .get("id")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        content_index,
    })
}

fn append_response_delta(
    output: &mut String,
    event_type: &str,
    delta: &str,
    identity: Option<ResponseOutputItemIdentity>,
) {
    let Some(identity) = identity else {
        return;
    };
    let mut event = serde_json::json!({
        "type": event_type,
        "delta": delta,
        "output_index": identity.output_index
    });
    if let Some(item_id) = identity.item_id {
        event["item_id"] = serde_json::Value::String(item_id);
    }
    if let Some(content_index) = identity.content_index {
        event["content_index"] = serde_json::json!(content_index);
    }
    output.push_str("event: ");
    output.push_str(event_type);
    output.push_str("\ndata: ");
    output.push_str(&event.to_string());
    output.push_str("\n\n");
}

fn add_reasoning_output_item(
    output: &mut String,
    response_id: &str,
    added: &mut bool,
    output_index: &mut Option<usize>,
    next_output_index: &mut usize,
) {
    if *added {
        return;
    }

    let index = *next_output_index;
    *next_output_index += 1;
    let event = serde_json::json!({
        "type": "response.output_item.added",
        "output_index": index,
        "item": {
            "id": format!("rs_{}", response_id),
            "type": "reasoning",
            "summary": []
        }
    });
    output.push_str("event: response.output_item.added\ndata: ");
    output.push_str(&event.to_string());
    output.push_str("\n\n");
    *added = true;
    *output_index = Some(index);
}

fn convert_sse_payload_to_responses(
    payload: &str,
    preserved_response: Option<&serde_json::Value>,
) -> String {
    let mut converter = ResponsesStreamConverter::new(preserved_response.cloned());
    let mut output = String::new();
    for (event_type, data) in parse_sse_frames(payload) {
        if data.trim() == "[DONE]" {
            break;
        }
        output.push_str(&converter.push_frame(event_type.as_deref(), &data));
    }
    output.push_str(&converter.finish());
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_stream_converter_emits_each_frame_once() {
        let mut converter = ResponsesStreamConverter::new(None);
        let first = serde_json::json!({
            "id": "resp_incremental",
            "object": "chat.completion.chunk",
            "created": 42,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {"content": "first"},
                "finish_reason": null
            }]
        });
        let second = serde_json::json!({
            "id": "resp_incremental",
            "object": "chat.completion.chunk",
            "created": 42,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {"content": " second"},
                "finish_reason": "stop"
            }]
        });

        let first_events = converter.push_frame(None, &first.to_string());
        let second_events = converter.push_frame(None, &second.to_string());
        let terminal_events = converter.finish();

        assert!(first_events.contains("response.created"));
        assert!(first_events.contains("\"delta\":\"first\""));
        assert!(!second_events.contains("response.created"));
        assert!(!second_events.contains("\"delta\":\"first\""));
        assert!(second_events.contains("\"delta\":\" second\""));
        assert!(!second_events.contains("response.completed"));
        assert!(terminal_events.contains("response.output_item.done"));
        assert!(terminal_events.contains("response.completed"));
    }

    #[test]
    fn incremental_stream_converter_keeps_identity_and_usage_stable() {
        let mut converter = ResponsesStreamConverter::new(None);
        let first = serde_json::json!({
            "id": "resp_original",
            "object": "chat.completion.chunk",
            "created": 42,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {"content": "answer"},
                "finish_reason": null
            }],
            "usage": {
                "prompt_tokens": 11,
                "completion_tokens": 2,
                "total_tokens": 13
            }
        });
        let empty = serde_json::json!({
            "id": "resp_original",
            "object": "chat.completion.chunk",
            "created": 42,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {"content": ""},
                "finish_reason": null
            }],
            "usage": null
        });
        let late_message_start = serde_json::json!({
            "type": "message_start",
            "message": {"id": "resp_late", "model": "test-model"}
        });
        let usage_only = serde_json::json!({
            "id": "resp_original",
            "object": "chat.completion.chunk",
            "created": 42,
            "model": "test-model",
            "usage": {
                "prompt_tokens": 15,
                "completion_tokens": 4,
                "total_tokens": 19
            }
        });

        converter.push_frame(None, &first.to_string());
        let empty_events = converter.push_frame(None, &empty.to_string());
        converter.push_frame(Some("message_start"), &late_message_start.to_string());
        converter.push_frame(None, &usage_only.to_string());
        let terminal_events = converter.finish();

        assert!(!empty_events.contains("response.output_text.delta"));
        assert!(terminal_events.contains("\"id\":\"resp_original\""));
        assert!(!terminal_events.contains("resp_late"));
        assert!(terminal_events.contains("\"input_tokens\":15"));
        assert!(terminal_events.contains("\"output_tokens\":4"));
    }

    #[test]
    fn incremental_stream_converter_handles_anthropic_tool_use() {
        let mut converter = ResponsesStreamConverter::new(None);
        let message_start = serde_json::json!({
            "type": "message_start",
            "message": {
                "id": "msg_tools",
                "model": "test-model",
                "usage": {"input_tokens": 3, "output_tokens": 0}
            }
        });
        let tool_start = serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {
                "type": "tool_use",
                "id": "toolu_weather",
                "name": "weather",
                "input": {}
            }
        });
        let first_arguments = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": "{\"city\":\""}
        });
        let second_arguments = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": "Paris\"}"}
        });

        converter.push_frame(Some("message_start"), &message_start.to_string());
        let added = converter.push_frame(Some("content_block_start"), &tool_start.to_string());
        let first_delta =
            converter.push_frame(Some("content_block_delta"), &first_arguments.to_string());
        let second_delta =
            converter.push_frame(Some("content_block_delta"), &second_arguments.to_string());
        let terminal = converter.finish();

        assert!(added.contains("response.output_item.added"));
        assert!(added.contains("\"name\":\"weather\""));
        assert!(first_delta.contains("response.function_call_arguments.delta"));
        assert!(second_delta.contains("response.function_call_arguments.delta"));
        let done = parse_sse_frames(&terminal)
            .into_iter()
            .find(|(event, _)| event.as_deref() == Some("response.output_item.done"))
            .expect("tool stream should finish its output item");
        let done: serde_json::Value = serde_json::from_str(&done.1).unwrap();
        assert_eq!(done["item"]["id"], "toolu_weather");
        assert_eq!(done["item"]["arguments"], "{\"city\":\"Paris\"}");
    }

    #[tokio::test]
    async fn incremental_stream_failure_retains_active_response_id() {
        let first = format!(
            "data: {}\n\n",
            serde_json::json!({
                "id": "resp_active",
                "object": "chat.completion.chunk",
                "created": 42,
                "model": "test-model",
                "choices": [{
                    "index": 0,
                    "delta": {"content": "first"},
                    "finish_reason": null
                }]
            })
        );
        let body = Body::from_stream(futures::stream::iter([
            Ok::<Bytes, std::io::Error>(Bytes::from(first)),
            Ok(Bytes::from(vec![b'x'; MAX_RESPONSES_BODY_BYTES])),
        ]));
        let upstream = Response::builder()
            .status(StatusCode::OK)
            .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
            .body(body)
            .unwrap();

        let adapted = convert_openai_stream_response_to_responses(upstream).await;
        let body = axum::body::to_bytes(adapted.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let payload = String::from_utf8(body.to_vec()).unwrap();

        assert!(payload.contains("event: response.created"));
        assert!(payload.contains("event: response.failed"));
        assert!(payload.contains("\"id\":\"resp_active\""));
        assert!(!payload.contains("resp_failed"));
    }

    #[tokio::test]
    async fn responses_stream_rejects_encoded_upstream_body() {
        let upstream = Response::builder()
            .status(StatusCode::OK)
            .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
            .header(axum::http::header::CONTENT_ENCODING, "gzip")
            .body(Body::from("encoded bytes"))
            .unwrap();

        let adapted = convert_openai_stream_response_to_responses(upstream).await;

        assert_eq!(adapted.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(adapted.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert!(
            String::from_utf8_lossy(&body).contains("Unsupported upstream stream content encoding")
        );
    }

    #[test]
    fn continuation_fallback_omits_identity_metadata() {
        let item = serde_json::json!({
            "type": "mcp_approval_response",
            "approval_request_id": "approval_1",
            "approve": true
        });

        assert_eq!(normalize_continuation_output(&item), "{\"approve\":true}");
    }

    #[test]
    fn rejects_non_object_responses_reasoning() {
        let request = serde_json::json!({
            "model": "auto",
            "input": "Think",
            "reasoning": "high"
        });

        let error = responses_request_to_openai_chat_request(&request).unwrap_err();

        assert_eq!(error, "responses request 'reasoning' must be an object");
    }

    #[test]
    fn drops_null_previous_response_id() {
        let request = serde_json::json!({
            "model": "auto",
            "input": "Continue",
            "previous_response_id": null
        });

        let converted = responses_request_to_openai_chat_request(&request).unwrap();

        assert!(converted.get("previous_response_id").is_none());
    }

    #[test]
    fn accepts_native_continuation_results_for_passthrough() {
        let cases = [
            (
                "call_function",
                serde_json::json!({
                    "type": "function_call_output",
                    "call_id": "call_function",
                    "output": "function result"
                }),
            ),
            (
                "call_custom",
                serde_json::json!({
                    "type": "custom_tool_call_output",
                    "call_id": "call_custom",
                    "output": "custom result"
                }),
            ),
            (
                "call_computer",
                serde_json::json!({
                    "type": "computer_call_output",
                    "call_id": "call_computer",
                    "output": {
                        "type": "computer_screenshot",
                        "image_url": "data:image/png;base64,c2NyZWVuc2hvdA=="
                    }
                }),
            ),
            (
                "call_local_shell",
                serde_json::json!({
                    "type": "local_shell_call_output",
                    "id": "call_local_shell",
                    "output": "{\"stdout\":\"done\"}"
                }),
            ),
            (
                "call_shell",
                serde_json::json!({
                    "type": "shell_call_output",
                    "call_id": "call_shell",
                    "output": [{"stdout": "done", "outcome": {"type": "exit", "exit_code": 0}}]
                }),
            ),
            (
                "call_patch",
                serde_json::json!({
                    "type": "apply_patch_call_output",
                    "call_id": "call_patch",
                    "status": "completed"
                }),
            ),
            (
                "approval_1",
                serde_json::json!({
                    "type": "mcp_approval_response",
                    "approval_request_id": "approval_1",
                    "approve": true
                }),
            ),
        ];

        for (call_id, input) in cases {
            let request = serde_json::json!({
                "model": "auto",
                "previous_response_id": "resp_previous",
                "input": [input]
            });

            let converted = responses_request_to_openai_chat_request(&request).unwrap();

            assert_eq!(converted["messages"][0]["role"], "tool");
            assert_eq!(converted["messages"][0]["tool_call_id"], call_id);
            assert!(!converted["messages"][0]["content"]
                .as_str()
                .expect("continuation result should have a Chat-compatible representation")
                .is_empty());
            assert_eq!(converted["previous_response_id"], "resp_previous");
        }
    }

    #[test]
    fn chat_refusal_serializes_as_responses_refusal_block() {
        let openai = serde_json::json!({
            "id": "resp_refusal",
            "object": "chat.completion",
            "created": 42,
            "model": "muse",
            "response_status": "incomplete",
            "incomplete_details": {"reason": "content_filter"},
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "refusal": "I cannot help with that."
                },
                "finish_reason": "content_filter"
            }]
        });

        let response = openai_chat_completion_to_responses_json(&openai, None);

        assert_eq!(
            response["output"][0]["content"].as_array().unwrap().len(),
            1
        );
        assert_eq!(response["output"][0]["content"][0]["type"], "refusal");
        assert_eq!(
            response["output"][0]["content"][0]["refusal"],
            "I cannot help with that."
        );
        assert_eq!(response["status"], "incomplete");
        assert_eq!(
            response["incomplete_details"],
            serde_json::json!({"reason": "content_filter"})
        );
    }

    #[test]
    fn pseudo_stream_preserves_refusal_and_incomplete_status() {
        let payload = concat!(
            "data: {\"id\":\"resp_refusal\",\"object\":\"chat.completion.chunk\",",
            "\"created\":42,\"model\":\"muse\",\"response_status\":\"incomplete\",",
            "\"incomplete_details\":{\"reason\":\"content_filter\"},",
            "\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",",
            "\"refusal\":\"I cannot help with that.\"},\"finish_reason\":\"content_filter\"}]}\n\n",
            "data: [DONE]\n\n"
        );

        let converted = convert_sse_payload_to_responses(payload, None);
        let terminal = parse_sse_frames(&converted)
            .into_iter()
            .find(|(event, _)| event.as_deref() == Some("response.incomplete"))
            .expect("incomplete terminal event should be present");
        let terminal: serde_json::Value = serde_json::from_str(&terminal.1).unwrap();

        assert_eq!(terminal["response"]["status"], "incomplete");
        assert_eq!(
            terminal["response"]["incomplete_details"],
            serde_json::json!({"reason": "content_filter"})
        );
        assert_eq!(
            terminal["response"]["output"][0]["content"][0],
            serde_json::json!({
                "type": "refusal",
                "refusal": "I cannot help with that."
            })
        );
    }

    #[test]
    fn preserved_pseudo_stream_deltas_identify_their_output_items() {
        let preserved = serde_json::json!({
            "id": "resp_native",
            "object": "response",
            "created_at": 42,
            "status": "completed",
            "model": "muse",
            "output": [{
                "id": "rs_native",
                "type": "reasoning",
                "content": [{"type": "reasoning_text", "text": "think"}],
                "summary": []
            }, {
                "id": "fc_native",
                "type": "function_call",
                "call_id": "call_native",
                "name": "lookup",
                "arguments": "{\"q\":\"x\"}"
            }, {
                "id": "msg_native",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "answer"}]
            }]
        });
        let chunks = [
            serde_json::json!({
                "id": "resp_native",
                "object": "chat.completion.chunk",
                "created": 42,
                "model": "muse",
                "choices": [{"index": 0, "delta": {"reasoning_content": "think"}}]
            }),
            serde_json::json!({
                "id": "resp_native",
                "object": "chat.completion.chunk",
                "created": 42,
                "model": "muse",
                "choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": 0,
                    "id": "call_native",
                    "function": {"name": "lookup", "arguments": "{\"q\":\"x\"}"}
                }]}}]
            }),
            serde_json::json!({
                "id": "resp_native",
                "object": "chat.completion.chunk",
                "created": 42,
                "model": "muse",
                "choices": [{"index": 0, "delta": {"content": "answer"}}]
            }),
        ];
        let mut payload = chunks
            .iter()
            .map(|chunk| format!("data: {chunk}\n\n"))
            .collect::<String>();
        payload.push_str("data: [DONE]\n\n");

        let converted = convert_sse_payload_to_responses(&payload, Some(&preserved));
        let events = parse_sse_frames(&converted)
            .into_iter()
            .filter_map(|(_, data)| serde_json::from_str::<serde_json::Value>(&data).ok())
            .collect::<Vec<_>>();

        for (event_type, output_index, item_id) in [
            ("response.reasoning_text.delta", 0, "rs_native"),
            ("response.function_call_arguments.delta", 1, "fc_native"),
            ("response.output_text.delta", 2, "msg_native"),
        ] {
            let event = events
                .iter()
                .find(|event| event["type"] == event_type)
                .unwrap_or_else(|| panic!("missing {event_type}"));
            assert_eq!(event["output_index"], output_index);
            assert_eq!(event["item_id"], item_id);
        }
    }

    #[test]
    fn preserved_pseudo_stream_suppresses_ambiguous_flattened_text_deltas() {
        let ambiguous_outputs = [
            serde_json::json!([{
                "id": "msg_first",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "first"}]
            }, {
                "id": "msg_second",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "second"}]
            }]),
            serde_json::json!([{
                "id": "msg_multi_content",
                "type": "message",
                "role": "assistant",
                "content": [
                    {"type": "output_text", "text": "first"},
                    {"type": "output_text", "text": "second"}
                ]
            }]),
        ];
        let payload = concat!(
            "data: {\"id\":\"resp_native\",\"object\":\"chat.completion.chunk\",",
            "\"created\":42,\"model\":\"muse\",\"choices\":[{\"index\":0,",
            "\"delta\":{\"content\":\"first\\n\\nsecond\"}}]}\n\n",
            "data: [DONE]\n\n"
        );

        for output in ambiguous_outputs {
            let preserved = serde_json::json!({
                "id": "resp_native",
                "object": "response",
                "created_at": 42,
                "status": "completed",
                "model": "muse",
                "output": output
            });
            let converted = convert_sse_payload_to_responses(payload, Some(&preserved));
            let events = parse_sse_frames(&converted)
                .into_iter()
                .filter_map(|(_, data)| serde_json::from_str::<serde_json::Value>(&data).ok())
                .collect::<Vec<_>>();

            assert!(!events
                .iter()
                .any(|event| event["type"] == "response.output_text.delta"));
            let completed = events
                .iter()
                .find(|event| event["type"] == "response.completed")
                .expect("exact preserved terminal event should remain");
            assert_eq!(completed["response"], preserved);
        }
    }

    #[test]
    fn preserved_pseudo_stream_suppresses_flattened_reasoning_summaries() {
        let preserved = serde_json::json!({
            "id": "resp_native",
            "object": "response",
            "created_at": 42,
            "status": "completed",
            "model": "muse",
            "output": [{
                "id": "rs_summaries",
                "type": "reasoning",
                "summary": [
                    {"type": "summary_text", "text": "first"},
                    {"type": "summary_text", "text": "second"}
                ]
            }]
        });
        let payload = concat!(
            "data: {\"id\":\"resp_native\",\"object\":\"chat.completion.chunk\",",
            "\"created\":42,\"model\":\"muse\",\"choices\":[{\"index\":0,",
            "\"delta\":{\"reasoning_content\":\"first\\n\\nsecond\"}}]}\n\n",
            "data: [DONE]\n\n"
        );

        let converted = convert_sse_payload_to_responses(payload, Some(&preserved));
        let events = parse_sse_frames(&converted)
            .into_iter()
            .filter_map(|(_, data)| serde_json::from_str::<serde_json::Value>(&data).ok())
            .collect::<Vec<_>>();

        assert!(!events
            .iter()
            .any(|event| event["type"] == "response.reasoning_text.delta"));
        let completed = events
            .iter()
            .find(|event| event["type"] == "response.completed")
            .expect("exact preserved terminal event should remain");
        assert_eq!(completed["response"], preserved);
    }

    #[test]
    fn pseudo_stream_keeps_initial_response_id_and_terminal_status() {
        for (status, terminal_type) in [
            ("failed", "response.failed"),
            ("cancelled", "response.cancelled"),
        ] {
            let payload = format!(
                concat!(
                    "data: {{\"id\":\"resp_original\",\"object\":\"chat.completion.chunk\",",
                    "\"created\":42,\"model\":\"muse\",\"choices\":[{{\"index\":0,",
                    "\"delta\":{{\"role\":\"assistant\"}},\"finish_reason\":null}}]}}\n\n",
                    "data: {{\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",",
                    "\"created\":42,\"model\":\"muse\",\"response_status\":\"{}\",",
                    "\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"done\"}},",
                    "\"finish_reason\":\"stop\"}}]}}\n\n",
                    "data: [DONE]\n\n"
                ),
                status
            );

            let converted = convert_sse_payload_to_responses(&payload, None);
            let frames = parse_sse_frames(&converted);
            let created: serde_json::Value = serde_json::from_str(&frames[0].1).unwrap();
            let terminal = frames
                .iter()
                .find(|(event, _)| event.as_deref() == Some(terminal_type))
                .unwrap();
            let terminal: serde_json::Value = serde_json::from_str(&terminal.1).unwrap();

            assert_eq!(created["response"]["id"], "resp_original");
            assert_eq!(terminal["response"]["id"], "resp_original");
            assert_eq!(terminal["response"]["status"], status);
            assert_eq!(terminal["response"]["output"][0]["id"], "msg_resp_original");
        }
    }
}
