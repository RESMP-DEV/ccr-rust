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
            input_tokens_details: None,
        }),
        extra_data: response.reasoning_content.map(|rc| {
            serde_json::json!({ "reasoning_content": rc })
        }),
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

        loop {
            tokio::select! {
                chunk = stream.next() => {
                    let Some(chunk_res) = chunk else { break; };
                    match chunk_res {
                        Ok(bytes) => {
                            for frame in decoder.push(&bytes) {
                                let data = frame.data;
                                let event_type = frame.event;

                                if data.trim() == "[DONE]" {
                                    if !sent_done {
                                        let _ = tx.send(Ok(Bytes::from("data: [DONE]\n\n"))).await;
                                        sent_done = true;
                                    }
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

                                let transformed: serde_json::Value = match transformer.transform_response(event_json) {
                                    Ok(value) => value,
                                    Err(_) => {
                                        let msg = format!("data: {}\n\n", data);
                                        let _ = tx.send(Ok(Bytes::from(msg))).await;
                                        continue;
                                    }
                                };

                                let msg = format!("data: {}\n\n", serde_json::to_string(&transformed).unwrap_or_default());
                                if tx.send(Ok(Bytes::from(msg))).await.is_err() {
                                    return; // Receiver closed
                                }

                                if event_type.as_deref() == Some("message_stop") && !sent_done {
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
pub async fn handle_chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request_body): Json<serde_json::Value>,
) -> Response {
    // Preserve the original OpenAI-formatted body for potential passthrough
    // to OpenAI-compatible backends (avoids OpenAI→Anthropic→OpenAI round-trip).
    let passthrough_body = request_body.clone();

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
