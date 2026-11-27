// SPDX-License-Identifier: AGPL-3.0-or-later
use super::super::types::*;
use super::{send_stream_timeout_error, BoxByteStream};
use crate::metrics::{
    record_cost, record_failure, record_stream_backpressure, record_throughput, record_ttft,
    record_usage, verify_token_usage, ActiveStreamGuard,
};
use crate::sse::{SseFrameDecoder, StreamVerifyCtx};
use crate::transformer::TransformerChain;
use axum::body::Body;
use axum::http::StatusCode;
use axum::response::Response;
use bytes::Bytes;
use futures::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tracing::warn;

/// Stream Anthropic protocol response with token tracking.
///
/// Parses Anthropic SSE events (`message_delta` with usage) to track tokens.
/// Used for Anthropic-protocol providers that may not return token counts.
pub async fn stream_anthropic_response_with_tracking(
    byte_stream: BoxByteStream,
    buffer_size: usize,
    verify_ctx: StreamVerifyCtx,
    chain: TransformerChain,
) -> Response {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(buffer_size);
    let tier_name = verify_ctx.tier_name.clone();
    let local_estimate = verify_ctx.local_estimate;
    let pricing = verify_ctx.pricing;

    tokio::spawn(async move {
        let _active_stream_guard = ActiveStreamGuard::new();
        let mut stream = byte_stream;
        let mut decoder = SseFrameDecoder::new();
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;
        let mut cache_read_tokens: u64 = 0;
        let mut cache_creation_tokens: u64 = 0;
        let mut accumulated_content_len: usize = 0;
        // TTFT/throughput timing
        let stream_start = verify_ctx.stream_start;
        let idle_timeout = verify_ctx.stream_idle_timeout;
        let mut idle_deadline = tokio::time::Instant::now() + idle_timeout;
        let mut first_token_time: Option<std::time::Instant> = None;
        let mut last_token_time: Option<std::time::Instant> = None;
        let mut ended_with_timeout = false;
        let mut saw_message_stop = false;
        let mut upstream_completed_cleanly = true;

        macro_rules! handle_anthropic_frame {
            ($frame:expr, $forwarded:ident) => {{
                let frame = $frame;
                let json_str = frame.data.trim();
                if json_str == "[DONE]" || json_str.is_empty() {
                    continue;
                }

                let event_kind = serde_json::from_str::<serde_json::Value>(json_str)
                    .ok()
                    .and_then(|event| {
                        event
                            .get("type")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    });
                saw_message_stop |= frame.event.as_deref() == Some("message_stop")
                    || event_kind.as_deref() == Some("message_stop");

                // Parse Anthropic SSE events to extract usage.
                // message_start reports usage under
                // `message.usage` (input plus cache splits);
                // message_delta reports a top-level `usage`
                // (cumulative output, and input or cache fields
                // on some providers). Positive values win so a
                // later event never zeroes an earlier report.
                if let Ok(event) = serde_json::from_str::<serde_json::Value>(json_str) {
                    let usage_sources = [
                        event.get("usage"),
                        event.get("message").and_then(|m| m.get("usage")),
                    ];
                    for usage in usage_sources.into_iter().flatten() {
                        if let Some(input) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                            if input > 0 {
                                input_tokens = input;
                            }
                        }
                        if let Some(output) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                            if output > 0 {
                                output_tokens = output;
                            }
                        }
                        if let Some(read) = usage
                            .get("cache_read_input_tokens")
                            .and_then(|v| v.as_u64())
                        {
                            if read > 0 {
                                cache_read_tokens = read;
                            }
                        }
                        if let Some(creation) = usage
                            .get("cache_creation_input_tokens")
                            .and_then(|v| v.as_u64())
                        {
                            if creation > 0 {
                                cache_creation_tokens = creation;
                            }
                        }
                    }

                    // Also track content length for estimation fallback
                    if let Some(delta) = event.get("delta") {
                        if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                            if first_token_time.is_none() && !text.is_empty() {
                                first_token_time = Some(std::time::Instant::now());
                            }
                            if !text.is_empty() {
                                last_token_time = Some(std::time::Instant::now());
                            }
                            accumulated_content_len += text.len();
                        }
                    }
                    if let Some(content_block) = event.get("content_block") {
                        if let Some(text) = content_block.get("text").and_then(|v| v.as_str()) {
                            if first_token_time.is_none() && !text.is_empty() {
                                first_token_time = Some(std::time::Instant::now());
                            }
                            if !text.is_empty() {
                                last_token_time = Some(std::time::Instant::now());
                            }
                            accumulated_content_len += text.len();
                        }
                    }
                }

                // Apply in-process response transformers if the
                // chain is not empty. External plugins are
                // skipped per frame; see `apply_response_frame`.
                let output_frame = if !chain.is_empty() {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) {
                        if let Ok(transformed) = chain.apply_response_frame(value) {
                            serde_json::to_string(&transformed)
                                .unwrap_or_else(|_| json_str.to_string())
                        } else {
                            json_str.to_string()
                        }
                    } else {
                        json_str.to_string()
                    }
                } else {
                    json_str.to_string()
                };

                // Reconstruct SSE frame
                let sse_data = if let Some(event_type) = &frame.event {
                    format!("event: {}\ndata: {}\n\n", event_type, output_frame)
                } else {
                    format!("data: {}\n\n", output_frame)
                };

                if tx.capacity() == 0 {
                    record_stream_backpressure();
                }
                if tx.send(Ok(Bytes::from(sse_data))).await.is_err() {
                    upstream_completed_cleanly = false;
                    break;
                }
                $forwarded = true;
            }};
        }

        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(idle_deadline) => {
                    warn!(
                        tier = %tier_name,
                        idle_timeout_ms = idle_timeout.as_millis(),
                        "CCR stream idle timeout"
                    );
                    record_failure(&tier_name, "stream_idle_timeout");
                    ended_with_timeout = true;
                    break;
                }
                chunk = stream.next() => {
                    let Some(chunk) = chunk else {
                        break;
                    };
                    match chunk {
                        Ok(bytes) => {
                            let mut forwarded = false;
                            for frame in decoder.push(&bytes) {
                                handle_anthropic_frame!(frame, forwarded);
                            }
                            if forwarded {
                                idle_deadline = tokio::time::Instant::now() + idle_timeout;
                            }
                        }
                        Err(e) => {
                            upstream_completed_cleanly = false;
                            // Recover the pending final frame before the
                            // transport error reaches the client, so a
                            // newline-terminated tail is not dropped.
                            let mut finish_forwarded;
                            for frame in decoder.finish() {
                                handle_anthropic_frame!(frame, finish_forwarded);
                                let _ = finish_forwarded;
                            }
                            let _ = tx.send(Err(std::io::Error::other(e.to_string()))).await;
                            break;
                        }
                    }
                }
                _ = tx.closed() => {
                    tracing::debug!("Client disconnected, aborting Anthropic upstream");
                    upstream_completed_cleanly = false;
                    break;
                }
            }
        }

        // Every end-of-stream branch flushes the decoder: `push` only emits
        // blank-line-terminated frames, so a newline-terminated tail is
        // recovered here instead of being dropped.
        let mut finish_forwarded;
        for frame in decoder.finish() {
            handle_anthropic_frame!(frame, finish_forwarded);
            let _ = finish_forwarded;
        }

        if ended_with_timeout {
            send_stream_timeout_error(&tx, &tier_name, idle_timeout).await;
            return;
        }

        upstream_completed_cleanly &= saw_message_stop;

        // Anthropic-style usage reports the uncached prompt slice in
        // input_tokens with cache reads and writes as separate fields.
        // Account the full prompt volume and keep the cache splits for
        // reporting and discounted cost estimation.
        let raw_prompt_tokens = input_tokens
            .saturating_add(cache_read_tokens)
            .saturating_add(cache_creation_tokens);

        // Estimate tokens if provider returned zeros (some Anthropic-compatible APIs don't report usage)
        let final_input_tokens = if raw_prompt_tokens == 0 && local_estimate > 0 {
            warn!(
                tier = %tier_name,
                "Anthropic stream returned 0 input_tokens, using pre-request estimate: {}",
                local_estimate
            );
            local_estimate
        } else {
            raw_prompt_tokens
        };

        let final_output_tokens = if output_tokens == 0 && accumulated_content_len > 0 {
            let estimate = (accumulated_content_len / 4).max(1) as u64;
            warn!(
                tier = %tier_name,
                "Anthropic stream returned 0 output_tokens, estimated: {} (from {} chars)",
                estimate,
                accumulated_content_len
            );
            estimate
        } else {
            output_tokens
        };

        // Record usage. Drift verification receives the raw upstream value:
        // when the provider omitted usage (raw_prompt_tokens == 0) there is
        // nothing to compare, and passing the substituted estimate would
        // record a false 0% drift sample.
        record_usage(
            &tier_name,
            final_input_tokens,
            final_output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
        );
        verify_token_usage(&tier_name, local_estimate, raw_prompt_tokens);
        if let Some(cost) = pricing.and_then(|p| {
            p.estimate_request_cost_usd_with_cache(
                final_input_tokens
                    .saturating_sub(cache_read_tokens)
                    .saturating_sub(cache_creation_tokens),
                cache_read_tokens,
                cache_creation_tokens,
                final_output_tokens,
            )
        }) {
            record_cost(&tier_name, cost);
        }

        // Record TTFT and throughput metrics
        let ttft_secs = first_token_time
            .map(|ft| ft.duration_since(stream_start).as_secs_f64())
            .unwrap_or(0.0);
        if ttft_secs > 0.0 {
            record_ttft(&tier_name, ttft_secs);
        }
        if let (Some(ft), Some(lt)) = (first_token_time, last_token_time) {
            let gen_secs = lt.duration_since(ft).as_secs_f64();
            if gen_secs > 0.0 && final_output_tokens > 0 {
                record_throughput(&tier_name, final_output_tokens, gen_secs, ttft_secs);
            }
        }

        // Update rate limit state; a mid-stream upstream error or client
        // disconnect is not a success signal for the route.
        if upstream_completed_cleanly {
            if let Some(permit) = verify_ctx.admission_permit.as_ref() {
                permit.record_success();
            }
            if let Some(ref tracker) = verify_ctx.ratelimit_tracker {
                if let Some((remaining, reset_at)) = &verify_ctx.rate_limit_info {
                    tracker.record_success_if_current(
                        &verify_ctx.ratelimit_key,
                        verify_ctx.ratelimit_generation,
                        *remaining,
                        *reset_at,
                    );
                } else {
                    tracker.record_success_if_current(
                        &verify_ctx.ratelimit_key,
                        verify_ctx.ratelimit_generation,
                        None,
                        None,
                    );
                }
            }
        }
    });

    let body = Body::from_stream(ReceiverStream::new(rx));

    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
        .header(axum::http::header::CACHE_CONTROL, "no-cache")
        .header(axum::http::header::CONNECTION, "keep-alive")
        .body(body)
        .unwrap()
}

// ============================================================================
// Pseudo-Streaming (non-streaming → SSE)
// ============================================================================

/// Emit a complete Anthropic response as a sequence of SSE events.
///
/// Used when `forceNonStreaming` is true but the client requested `stream: true`.
pub(super) fn emit_anthropic_sse_events(
    resp: &AnthropicResponse,
    include_unsigned_reasoning: bool,
) -> Vec<String> {
    let mut events = Vec::new();
    let reasoning = include_unsigned_reasoning
        .then_some(resp.reasoning_content.as_deref())
        .flatten()
        .filter(|reasoning| !reasoning.trim().is_empty())
        .filter(|_| {
            !resp
                .content
                .iter()
                .any(|block| matches!(block, AnthropicContentBlock::Thinking { .. }))
        });
    let reasoning_offset = usize::from(reasoning.is_some());

    // message_start
    let mut start_usage = serde_json::json!({
        "input_tokens": resp.usage.input_tokens,
        "output_tokens": 0
    });
    if let Some(cached_tokens) = resp.usage.cache_read_input_tokens {
        start_usage["cache_read_input_tokens"] = serde_json::json!(cached_tokens);
    }
    if let Some(created_tokens) = resp.usage.cache_creation_input_tokens {
        start_usage["cache_creation_input_tokens"] = serde_json::json!(created_tokens);
    }
    let mut message = serde_json::json!({
        "id": resp.id,
        "type": "message",
        "role": "assistant",
        "content": [],
        "model": resp.model,
        "stop_reason": null,
        "stop_sequence": null,
        "usage": start_usage
    });
    if let Some(refusal) = &resp.refusal {
        message["refusal"] = serde_json::Value::String(refusal.clone());
    }
    if let Some(response_status) = &resp.response_status {
        message["response_status"] = serde_json::Value::String(response_status.clone());
    }
    if let Some(incomplete_details) = &resp.incomplete_details {
        message["incomplete_details"] = incomplete_details.clone();
    }
    let start_msg = serde_json::json!({
        "type": "message_start",
        "message": message
    });
    events.push(format!("event: message_start\ndata: {}\n\n", start_msg));

    if let Some(reasoning) = reasoning {
        let block_start = serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "thinking", "thinking": ""}
        });
        let delta = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "thinking_delta", "thinking": reasoning}
        });
        let block_stop = serde_json::json!({"type": "content_block_stop", "index": 0});
        events.push(format!(
            "event: content_block_start\ndata: {}\n\n",
            block_start
        ));
        events.push(format!("event: content_block_delta\ndata: {}\n\n", delta));
        events.push(format!(
            "event: content_block_stop\ndata: {}\n\n",
            block_stop
        ));
    }

    // Emit content blocks (text, tool_use, and thinking).
    for (content_idx, block) in resp.content.iter().enumerate() {
        let idx = content_idx + reasoning_offset;
        match block {
            AnthropicContentBlock::Text { text } => {
                // content_block_start
                let block_start = serde_json::json!({
                    "type": "content_block_start",
                    "index": idx,
                    "content_block": {"type": "text", "text": ""}
                });
                events.push(format!(
                    "event: content_block_start\ndata: {}\n\n",
                    block_start
                ));

                // content_block_delta
                let delta = serde_json::json!({
                    "type": "content_block_delta",
                    "index": idx,
                    "delta": {"type": "text_delta", "text": text}
                });
                events.push(format!("event: content_block_delta\ndata: {}\n\n", delta));
            }
            AnthropicContentBlock::ToolUse { id, name, input } => {
                // content_block_start (with tool metadata, empty input)
                let block_start = serde_json::json!({
                    "type": "content_block_start",
                    "index": idx,
                    "content_block": {
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": {}
                    }
                });
                events.push(format!(
                    "event: content_block_start\ndata: {}\n\n",
                    block_start
                ));

                // content_block_delta (full input as single input_json_delta)
                let input_json = serde_json::to_string(input).unwrap_or_default();
                let delta = serde_json::json!({
                    "type": "content_block_delta",
                    "index": idx,
                    "delta": {"type": "input_json_delta", "partial_json": input_json}
                });
                events.push(format!("event: content_block_delta\ndata: {}\n\n", delta));
            }
            AnthropicContentBlock::RedactedThinking { data } => {
                // Redacted thinking arrives whole and emits no deltas; only
                // the block start carries the opaque payload.
                let block_start = serde_json::json!({
                    "type": "content_block_start",
                    "index": idx,
                    "content_block": {"type": "redacted_thinking", "data": data}
                });
                events.push(format!(
                    "event: content_block_start\ndata: {}\n\n",
                    block_start
                ));
            }
            AnthropicContentBlock::Thinking {
                thinking,
                signature,
            } => {
                // content_block_start
                let block_start = serde_json::json!({
                    "type": "content_block_start",
                    "index": idx,
                    "content_block": {"type": "thinking", "thinking": ""}
                });
                events.push(format!(
                    "event: content_block_start\ndata: {}\n\n",
                    block_start
                ));

                // content_block_delta
                let delta = serde_json::json!({
                    "type": "content_block_delta",
                    "index": idx,
                    "delta": {"type": "thinking_delta", "thinking": thinking}
                });
                events.push(format!("event: content_block_delta\ndata: {}\n\n", delta));

                // signature_delta
                let sig_delta = serde_json::json!({
                    "type": "content_block_delta",
                    "index": idx,
                    "delta": {"type": "signature_delta", "signature": signature}
                });
                events.push(format!(
                    "event: content_block_delta\ndata: {}\n\n",
                    sig_delta
                ));
            }
        }

        // content_block_stop (common to all block types)
        let stop = serde_json::json!({
            "type": "content_block_stop",
            "index": idx
        });
        events.push(format!("event: content_block_stop\ndata: {}\n\n", stop));
    }

    // message_delta
    let mut delta_usage = serde_json::json!({
        "output_tokens": resp.usage.output_tokens
    });
    if let Some(reasoning_tokens) = resp.usage.reasoning_tokens {
        delta_usage["reasoning_tokens"] = serde_json::json!(reasoning_tokens);
    }
    let msg_delta = serde_json::json!({
        "type": "message_delta",
        "delta": {
            "stop_reason": resp.stop_reason,
            "stop_sequence": null
        },
        "usage": delta_usage
    });
    events.push(format!("event: message_delta\ndata: {}\n\n", msg_delta));

    let message_stop = serde_json::json!({"type": "message_stop"});
    events.push(format!("event: message_stop\ndata: {}\n\n", message_stop));

    events
}
