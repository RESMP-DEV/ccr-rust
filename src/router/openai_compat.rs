// SPDX-License-Identifier: AGPL-3.0-or-later
// OpenAI Chat Completions compatibility layer.
//
// Converts between Anthropic format and OpenAI chat completions format.
// This handles the `/v1/chat/completions` endpoint.

use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use futures::StreamExt;
use std::collections::HashMap;
use tokio_stream::wrappers::ReceiverStream;
use tracing::error;

use crate::frontend::codex::CodexFrontend;
use crate::frontend::Frontend;
use crate::metrics::increment_active_streams;
use crate::sse::SseFrameDecoder;
use crate::transform::anthropic_to_openai::AnthropicToOpenAiResponseTransformer;
use crate::transformer::Transformer;

use super::{
    handle_messages, AnthropicContentBlock, AnthropicRequest, AnthropicResponse, AppState, Message,
};

pub(super) fn internal_request_to_anthropic_request(
    req: crate::frontend::InternalRequest,
) -> AnthropicRequest {
    AnthropicRequest {
        model: req.model,
        messages: req
            .messages
            .into_iter()
            .map(|m| Message {
                role: m.role,
                content: m.content,
                tool_call_id: m.tool_call_id,
            })
            .collect(),
        system: req.system,
        max_tokens: req.max_tokens,
        temperature: req.temperature,
        stream: req.stream,
        tools: req.tools.map(|tools| {
            tools
                .into_iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema.unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}))
                    })
                })
                .collect()
        }),
        openai_passthrough_body: None,
    }
}

pub(super) fn anthropic_response_to_internal(
    response: AnthropicResponse,
) -> crate::frontend::InternalResponse {
    let content = response
        .content
        .into_iter()
        .map(|block| match block {
            AnthropicContentBlock::Text { text } => crate::frontend::ContentBlock::Text { text },
            AnthropicContentBlock::Thinking {
                thinking,
                signature,
            } => crate::frontend::ContentBlock::Thinking {
                thinking,
                signature: if signature.is_empty() {
                    None
                } else {
                    Some(signature)
                },
            },
            AnthropicContentBlock::ToolUse { id, name, input } => {
                crate::frontend::ContentBlock::ToolUse { id, name, input }
            }
        })
        .collect();

    let mut extra_data = serde_json::Map::new();
    if let Some(reasoning_content) = response.reasoning_content {
        extra_data.insert(
            "reasoning_content".to_string(),
            serde_json::Value::String(reasoning_content),
        );
    }
    if let Some(refusal) = response.refusal {
        extra_data.insert("refusal".to_string(), serde_json::Value::String(refusal));
    }
    if let Some(response_status) = response.response_status {
        extra_data.insert(
            "response_status".to_string(),
            serde_json::Value::String(response_status),
        );
    }
    if let Some(incomplete_details) = response.incomplete_details {
        extra_data.insert("incomplete_details".to_string(), incomplete_details);
    }
    if let Some(responses_output) = response.responses_output {
        extra_data.insert(
            super::RESPONSES_OUTPUT_PASSTHROUGH_KEY.to_string(),
            responses_output,
        );
    }

    crate::frontend::InternalResponse {
        id: response.id,
        response_type: response.response_type,
        role: response.role,
        model: response.model,
        content,
        stop_reason: response.stop_reason,
        usage: Some(crate::frontend::Usage {
            input_tokens: response.usage.input_tokens,
            output_tokens: response.usage.output_tokens,
            input_tokens_details: response
                .usage
                .cache_read_input_tokens
                .map(|cached_tokens| serde_json::json!({"cached_tokens": cached_tokens})),
            output_tokens_details: response
                .usage
                .reasoning_tokens
                .map(|reasoning_tokens| serde_json::json!({"reasoning_tokens": reasoning_tokens})),
        }),
        extra_data: (!extra_data.is_empty()).then_some(serde_json::Value::Object(extra_data)),
    }
}

async fn convert_anthropic_json_response_to_openai(response: Response) -> Response {
    let (mut parts, body) = response.into_parts();
    let body_bytes = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(err) => {
            error!("Failed to read Anthropic response body: {}", err);
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": "Failed to read upstream response"})),
            )
                .into_response();
        }
    };

    if parts.status != StatusCode::OK {
        // Normalize rate limit errors
        if parts.status == StatusCode::TOO_MANY_REQUESTS {
            if let Ok(mut error_json) = serde_json::from_slice::<serde_json::Value>(&body_bytes) {
                if let Some(error_obj) = error_json.get_mut("error").and_then(|e| e.as_object_mut())
                {
                    error_obj.insert(
                        "type".to_string(),
                        serde_json::Value::String("rate_limit_error".to_string()),
                    );
                    error_obj.insert(
                        "code".to_string(),
                        serde_json::Value::String("rate_limited".to_string()),
                    );

                    if let Some(retry_after) = parts
                        .headers
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<i64>().ok())
                    {
                        error_obj.insert("retry_after".to_string(), serde_json::json!(retry_after));
                    }
                }

                return Response::from_parts(
                    parts,
                    Body::from(serde_json::to_vec(&error_json).unwrap_or(body_bytes.to_vec())),
                );
            }
        }
        return Response::from_parts(parts, Body::from(body_bytes));
    }

    let anthropic_response: AnthropicResponse = match serde_json::from_slice(&body_bytes) {
        Ok(resp) => resp,
        Err(_) => {
            // If the payload is not Anthropic-shaped, pass through unchanged.
            return Response::from_parts(parts, Body::from(body_bytes));
        }
    };

    let internal_response = anthropic_response_to_internal(anthropic_response);
    let frontend = CodexFrontend::new();
    let serialized = match frontend.serialize_response(internal_response) {
        Ok(bytes) => bytes,
        Err(err) => {
            error!("Failed to serialize OpenAI response: {}", err);
            return Response::from_parts(parts, Body::from(body_bytes));
        }
    };

    parts.headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    Response::from_parts(parts, Body::from(serialized))
}

fn remap_anthropic_tool_call_index(
    event: &mut serde_json::Value,
    tool_indices: &mut HashMap<u64, u64>,
    next_tool_index: &mut u64,
) {
    let event_type = event.get("type").and_then(serde_json::Value::as_str);
    let block_index = event.get("index").and_then(serde_json::Value::as_u64);
    let Some(block_index) = block_index else {
        return;
    };

    let is_tool_start = event_type == Some("content_block_start")
        && event
            .get("content_block")
            .and_then(|block| block.get("type"))
            .and_then(serde_json::Value::as_str)
            == Some("tool_use");
    if is_tool_start {
        let dense_index = *tool_indices.entry(block_index).or_insert_with(|| {
            let index = *next_tool_index;
            *next_tool_index += 1;
            index
        });
        event["index"] = serde_json::json!(dense_index);
        return;
    }

    let is_tool_delta = event_type == Some("content_block_delta")
        && event
            .get("delta")
            .and_then(|delta| delta.get("partial_json"))
            .is_some();
    if is_tool_delta {
        if let Some(dense_index) = tool_indices.get(&block_index) {
            event["index"] = serde_json::json!(dense_index);
        }
    }
}

async fn convert_anthropic_stream_response_to_openai(response: Response) -> Response {
    let (mut parts, body) = response.into_parts();

    if parts.status != StatusCode::OK {
        // For error responses, read the whole body (it's likely small)
        let body_bytes = match to_bytes(body, usize::MAX).await {
            Ok(bytes) => bytes,
            Err(_) => return Response::from_parts(parts, Body::empty()),
        };

        // Normalize rate limit errors
        if parts.status == StatusCode::TOO_MANY_REQUESTS {
            if let Ok(mut error_json) = serde_json::from_slice::<serde_json::Value>(&body_bytes) {
                if let Some(error_obj) = error_json.get_mut("error").and_then(|e| e.as_object_mut())
                {
                    error_obj.insert(
                        "type".to_string(),
                        serde_json::Value::String("rate_limit_error".to_string()),
                    );
                    error_obj.insert(
                        "code".to_string(),
                        serde_json::Value::String("rate_limited".to_string()),
                    );

                    if let Some(retry_after) = parts
                        .headers
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<i64>().ok())
                    {
                        error_obj.insert("retry_after".to_string(), serde_json::json!(retry_after));
                    }
                }

                return Response::from_parts(
                    parts,
                    Body::from(serde_json::to_vec(&error_json).unwrap_or(body_bytes.to_vec())),
                );
            }
        }

        return Response::from_parts(parts, Body::from(body_bytes));
    }

    parts.headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/event-stream"),
    );

    // Create a channel for streaming response
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(100);

    increment_active_streams(1);

    tokio::spawn(async move {
        let mut stream = body.into_data_stream();
        let mut decoder = SseFrameDecoder::new();
        let transformer = AnthropicToOpenAiResponseTransformer;
        let mut sent_done = false;
        let mut prompt_tokens = None;
        let mut completion_tokens = None;
        let mut cached_tokens = None;
        let mut reasoning_tokens = None;
        let mut tool_indices = HashMap::new();
        let mut next_tool_index = 0;
        let mut response_id = None;
        let mut response_model = None;

        loop {
            tokio::select! {
                chunk = stream.next() => {
                    let Some(chunk_res) = chunk else { break; };
                    match chunk_res {
                        Ok(bytes) => {
                            for frame in decoder.push(&bytes) {
                                let data = frame.data;
                                let event_type = frame.event;

                                // The first terminal marker closes the OpenAI stream. Ignore
                                // duplicate or trailing Anthropic frames rather than emitting
                                // data after [DONE].
                                if sent_done {
                                    continue;
                                }

                                if data.trim() == "[DONE]" {
                                    let _ = tx.send(Ok(Bytes::from("data: [DONE]\n\n"))).await;
                                    sent_done = true;
                                    continue;
                                }

                                let mut event_json: serde_json::Value = match serde_json::from_str(&data) {
                                    Ok(value) => value,
                                    Err(_) => {
                                        // Pass through raw data if parsing fails
                                        let msg = format!("data: {}\n\n", data);
                                        let _ = tx.send(Ok(Bytes::from(msg))).await;
                                        continue;
                                    }
                                };

                                if event_json.get("type").is_none() {
                                    if let Some(t) = event_type.as_deref() {
                                        event_json["type"] = serde_json::Value::String(t.to_string());
                                    }
                                }

                                remap_anthropic_tool_call_index(
                                    &mut event_json,
                                    &mut tool_indices,
                                    &mut next_tool_index,
                                );

                                let event_kind = event_json
                                    .get("type")
                                    .and_then(serde_json::Value::as_str)
                                    .map(str::to_string);
                                if event_kind.as_deref() == Some("message_start") {
                                    response_id = event_json["message"]
                                        .get("id")
                                        .and_then(serde_json::Value::as_str)
                                        .map(str::to_string);
                                    response_model = event_json["message"]
                                        .get("model")
                                        .and_then(serde_json::Value::as_str)
                                        .map(str::to_string);
                                }
                                let event_usage = match event_kind.as_deref() {
                                    Some("message_start") => Some(&event_json["message"]["usage"]),
                                    Some("message_delta" | "message_stop") => {
                                        Some(&event_json["usage"])
                                    }
                                    _ => None,
                                };
                                if let Some(event_usage) = event_usage {
                                    prompt_tokens = event_usage
                                        .get("input_tokens")
                                        .and_then(serde_json::Value::as_u64)
                                        .or(prompt_tokens);
                                    completion_tokens = event_usage
                                        .get("output_tokens")
                                        .and_then(serde_json::Value::as_u64)
                                        .or(completion_tokens);
                                    cached_tokens = event_usage
                                        .get("cache_read_input_tokens")
                                        .and_then(serde_json::Value::as_u64)
                                        .or(cached_tokens);
                                    reasoning_tokens = event_usage
                                        .get("reasoning_tokens")
                                        .and_then(serde_json::Value::as_u64)
                                        .or(reasoning_tokens);
                                }

                                let mut transformed: serde_json::Value = match transformer.transform_response(event_json) {
                                    Ok(value) => value,
                                    Err(_) => {
                                        let msg = format!("data: {}\n\n", data);
                                        let _ = tx.send(Ok(Bytes::from(msg))).await;
                                        continue;
                                    }
                                };

                                if let Some(id) = response_id.as_deref() {
                                    transformed["id"] = serde_json::Value::String(id.to_string());
                                }
                                if let Some(model) = response_model.as_deref() {
                                    transformed["model"] = serde_json::Value::String(model.to_string());
                                }

                                if event_kind.as_deref() == Some("message_stop") && !sent_done {
                                    if let (Some(prompt), Some(completion)) =
                                        (prompt_tokens, completion_tokens)
                                    {
                                        let mut usage = serde_json::json!({
                                            "prompt_tokens": prompt,
                                            "completion_tokens": completion,
                                            "total_tokens": prompt.saturating_add(completion)
                                        });
                                        if let Some(cached) = cached_tokens {
                                            usage["prompt_tokens_details"] = serde_json::json!({
                                                "cached_tokens": cached
                                            });
                                        }
                                        if let Some(reasoning) = reasoning_tokens {
                                            usage["completion_tokens_details"] = serde_json::json!({
                                                "reasoning_tokens": reasoning
                                            });
                                        }
                                        transformed["usage"] = usage;
                                    }
                                }

                                let msg = format!("data: {}\n\n", serde_json::to_string(&transformed).unwrap_or_default());
                                if tx.send(Ok(Bytes::from(msg))).await.is_err() {
                                    return; // Receiver closed
                                }

                                if event_kind.as_deref() == Some("message_stop") && !sent_done {
                                    let _ = tx.send(Ok(Bytes::from("data: [DONE]\n\n"))).await;
                                    sent_done = true;
                                }
                            }
                        }
                        Err(e) => {
                            error!("Stream read error: {}", e);
                            break;
                        }
                    }
                }
                _ = tx.closed() => break,
            }
        }

        if !sent_done {
            let _ = tx.send(Ok(Bytes::from("data: [DONE]\n\n"))).await;
        }

        increment_active_streams(-1);
    });

    Response::from_parts(parts, Body::from_stream(ReceiverStream::new(rx)))
}

/// Handle OpenAI-format chat completion requests.
///
/// Converts to Anthropic format internally, processes the request,
/// then converts the response back to OpenAI format.
async fn handle_chat_completions_inner(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut request_body): Json<serde_json::Value>,
    native_responses_request: Option<serde_json::Value>,
) -> Response {
    if let Some(object) = request_body.as_object_mut() {
        object.remove(super::RESPONSES_REQUEST_PASSTHROUGH_KEY);
        object.remove(super::RESPONSES_OUTPUT_PASSTHROUGH_KEY);
    }

    // Preserve the original OpenAI-formatted body for potential passthrough
    // to OpenAI-compatible backends (avoids OpenAI→Anthropic→OpenAI round-trip).
    let mut passthrough_body = request_body.clone();
    if let Some(mut native_responses_request) = native_responses_request {
        if let Some(object) = native_responses_request.as_object_mut() {
            object.remove(super::RESPONSES_REQUEST_PASSTHROUGH_KEY);
            object.remove(super::RESPONSES_OUTPUT_PASSTHROUGH_KEY);
        }
        passthrough_body[super::RESPONSES_REQUEST_PASSTHROUGH_KEY] = native_responses_request;
    }

    let frontend = CodexFrontend::new();
    let internal_request = match frontend.parse_request(request_body) {
        Ok(req) => req,
        Err(e) => {
            error!("Failed to parse OpenAI request: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("Invalid request: {}", e)})),
            )
                .into_response();
        }
    };
    let stream_requested = internal_request.stream.unwrap_or(false);
    let mut anthropic_request = internal_request_to_anthropic_request(internal_request);
    anthropic_request.openai_passthrough_body = Some(passthrough_body);
    let response = handle_messages(State(state), headers, Json(anthropic_request)).await;

    if stream_requested {
        convert_anthropic_stream_response_to_openai(response).await
    } else {
        convert_anthropic_json_response_to_openai(response).await
    }
}

pub async fn handle_chat_completions(
    state: State<AppState>,
    headers: HeaderMap,
    request: Json<serde_json::Value>,
) -> Response {
    handle_chat_completions_inner(state, headers, request, None).await
}

pub(super) async fn handle_responses_chat_completions(
    state: State<AppState>,
    headers: HeaderMap,
    request: Json<serde_json::Value>,
    native_responses_request: serde_json::Value,
) -> Response {
    handle_chat_completions_inner(state, headers, request, Some(native_responses_request)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn duplicate_message_stop_does_not_emit_after_done() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\",\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\",\"usage\":{\"output_tokens\":2}}\n\n",
        );
        let response = Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(body))
            .unwrap();

        let converted = convert_anthropic_stream_response_to_openai(response).await;
        let bytes = to_bytes(converted.into_body(), usize::MAX).await.unwrap();
        let output = String::from_utf8(bytes.to_vec()).unwrap();

        assert_eq!(output.matches("data: [DONE]").count(), 1);
        assert_eq!(output.matches("\"total_tokens\":3").count(), 1);
        assert!(output.ends_with("data: [DONE]\n\n"));
    }

    #[tokio::test]
    async fn pseudo_stream_keeps_one_response_identity() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"resp_original\",\"model\":\"muse\",\"usage\":{\"input_tokens\":1}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\",\"usage\":{\"output_tokens\":1}}\n\n",
        );
        let response = Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(body))
            .unwrap();

        let converted = convert_anthropic_stream_response_to_openai(response).await;
        let bytes = to_bytes(converted.into_body(), usize::MAX).await.unwrap();
        let payload = String::from_utf8(bytes.to_vec()).unwrap();
        let ids: Vec<_> = payload
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter(|data| *data != "[DONE]")
            .map(|data| serde_json::from_str::<serde_json::Value>(data).unwrap()["id"].clone())
            .collect();

        assert!(!ids.is_empty());
        assert!(ids.iter().all(|id| id == "resp_original"));
    }

    #[tokio::test]
    async fn json_only_message_stop_terminates_before_later_frames() {
        let body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n",
            "data: {\"type\":\"message_stop\",\"usage\":{\"output_tokens\":2}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"late\"}}\n\n",
        );
        let response = Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(body))
            .unwrap();

        let converted = convert_anthropic_stream_response_to_openai(response).await;
        let bytes = to_bytes(converted.into_body(), usize::MAX).await.unwrap();
        let output = String::from_utf8(bytes.to_vec()).unwrap();

        assert_eq!(output.matches("data: [DONE]").count(), 1);
        assert!(!output.contains("late"));
        assert!(output.ends_with("data: [DONE]\n\n"));
    }

    #[test]
    fn anthropic_content_indices_map_to_dense_tool_indices() {
        let mut tool_indices = HashMap::new();
        let mut next_tool_index = 0;
        let mut first_start = serde_json::json!({
            "type": "content_block_start",
            "index": 2,
            "content_block": {"type": "tool_use", "id": "call_1"}
        });
        let mut first_delta = serde_json::json!({
            "type": "content_block_delta",
            "index": 2,
            "delta": {"type": "input_json_delta", "partial_json": "{}"}
        });
        let mut second_start = serde_json::json!({
            "type": "content_block_start",
            "index": 4,
            "content_block": {"type": "tool_use", "id": "call_2"}
        });

        remap_anthropic_tool_call_index(&mut first_start, &mut tool_indices, &mut next_tool_index);
        remap_anthropic_tool_call_index(&mut first_delta, &mut tool_indices, &mut next_tool_index);
        remap_anthropic_tool_call_index(&mut second_start, &mut tool_indices, &mut next_tool_index);

        assert_eq!(first_start["index"], 0);
        assert_eq!(first_delta["index"], 0);
        assert_eq!(second_start["index"], 1);
    }
}
