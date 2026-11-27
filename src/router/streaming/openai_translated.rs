// SPDX-License-Identifier: AGPL-3.0-or-later
use super::super::translate_response::{
    close_open_content_blocks, create_stream_stop_events, invalid_tool_arguments,
    translate_stream_chunk_to_anthropic, StreamTranslationState,
};
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
use tracing::{trace, warn};

// ============================================================================
// Streaming Response Translation
// ============================================================================

/// Emit the terminal Anthropic events for a translated OpenAI stream.
///
/// Every content block that is still open is closed exactly once, truncated
/// tool arguments are reported before the terminal `message_stop`, and the
/// message ends with `message_delta` plus `message_stop`. A stream that never
/// saw a finish reason reports `stop_reason: null`, so a conforming client can
/// tell the message did not complete normally.
async fn send_translated_terminal_events(
    tx: &tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
    state: &mut StreamTranslationState,
    usage: AnthropicUsage,
) {
    for event in close_open_content_blocks(state) {
        let event_json = serde_json::to_string(&event).unwrap_or_default();
        let sse_data = format!("event: {}\ndata: {}\n\n", event.event_type, event_json);
        let _ = tx.send(Ok(Bytes::from(sse_data))).await;
    }

    let invalid_arguments = invalid_tool_arguments(state);
    for (index, arguments) in &invalid_arguments {
        let payload = serde_json::json!({
            "type": "error",
            "error": {
                "type": "invalid_request_error",
                "message": format!(
                    "CCR stream ended with truncated tool arguments for content block {index} (accumulated {} bytes of invalid JSON)",
                    arguments.len()
                )
            }
        });
        let sse_data = format!("event: error\ndata: {}\n\n", payload);
        let _ = tx.send(Ok(Bytes::from(sse_data))).await;
    }

    // Tool arguments that never formed valid JSON mean the message did not
    // complete normally, so the terminal never reports a mapped stop reason.
    let finish_reason = if invalid_arguments.is_empty() {
        state.finish_reason.as_deref()
    } else {
        None
    };

    for event in create_stream_stop_events(Some(usage), finish_reason) {
        let event_json = serde_json::to_string(&event).unwrap_or_default();
        let sse_data = format!("event: {}\ndata: {}\n\n", event.event_type, event_json);
        let _ = tx.send(Ok(Bytes::from(sse_data))).await;
    }
}

/// Stream response with OpenAI -> Anthropic translation.
pub async fn stream_response_translated(
    byte_stream: BoxByteStream,
    buffer_size: usize,
    verify_ctx: Option<StreamVerifyCtx>,
    model_name: &str,
    chain: TransformerChain,
) -> Response {
    let _model_name = model_name.to_string();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(buffer_size);

    tokio::spawn(async move {
        let _active_stream_guard = ActiveStreamGuard::new();
        let mut stream = byte_stream;
        let mut decoder = SseFrameDecoder::new();
        let mut translation_state = StreamTranslationState::new();
        let mut accumulated_content = String::new();
        let mut accumulated_reasoning = String::new();
        let mut _has_reasoning = false;
        let mut accumulated_tool_calls: Vec<(String, String, String)> = Vec::new();
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;
        let mut cache_read_input_tokens: Option<u64> = None;
        let mut reasoning_tokens: Option<u64> = None;
        let stream_start = verify_ctx.as_ref().map(|ctx| ctx.stream_start);
        let tier_name = verify_ctx
            .as_ref()
            .map(|ctx| ctx.tier_name.clone())
            .unwrap_or_else(|| "unknown".to_string());
        let idle_timeout = verify_ctx
            .as_ref()
            .map(|ctx| ctx.stream_idle_timeout)
            .unwrap_or_else(|| std::time::Duration::from_secs(120));
        let mut idle_deadline = tokio::time::Instant::now() + idle_timeout;
        let mut first_token_time: Option<std::time::Instant> = None;
        let mut last_token_time: Option<std::time::Instant> = None;
        let mut ended_with_timeout = false;
        let mut saw_openai_terminal = false;
        let mut upstream_completed_cleanly = true;

        macro_rules! handle_openai_frame {
            ($frame:expr, $forwarded:ident) => {{
                let frame = $frame;
                let json_str = frame.data.trim();
                if json_str == "[DONE]" {
                    saw_openai_terminal = true;
                    continue;
                }
                if json_str.is_empty() {
                    continue;
                }

                // Try to parse as OpenAI stream chunk
                if let Ok(chunk) = serde_json::from_str::<OpenAIStreamChunk>(json_str) {
                    // Accumulate usage info
                    if let Some(ref usage) = chunk.usage {
                        input_tokens = usage.prompt_tokens;
                        output_tokens = usage.completion_tokens;
                        cache_read_input_tokens = usage
                            .prompt_tokens_details
                            .as_ref()
                            .and_then(|details| details.get("cached_tokens"))
                            .and_then(|value| value.as_u64())
                            .or(cache_read_input_tokens);
                        reasoning_tokens = usage
                            .completion_tokens_details
                            .as_ref()
                            .and_then(|details| details.get("reasoning_tokens"))
                            .and_then(|value| value.as_u64())
                            .or(reasoning_tokens);
                    }

                    let was_first = translation_state.is_first;
                    let events =
                        translate_stream_chunk_to_anthropic(&chunk, &mut translation_state);
                    saw_openai_terminal |= translation_state.finish_reason.is_some();
                    if was_first {
                        first_token_time = Some(std::time::Instant::now());
                    }
                    last_token_time = Some(std::time::Instant::now());

                    for event in events {
                        let event_json = serde_json::to_string(&event).unwrap_or_default();
                        let sse_data =
                            format!("event: {}\ndata: {}\n\n", event.event_type, event_json);

                        if tx.capacity() == 0 {
                            record_stream_backpressure();
                        }
                        if tx.send(Ok(Bytes::from(sse_data))).await.is_err() {
                            upstream_completed_cleanly = false;
                            break;
                        }
                        $forwarded = true;
                    }

                    if let Some(choice) = chunk.choices.first() {
                        if let Some(ref content) = choice.delta.content {
                            accumulated_content.push_str(content);
                        }
                        if let Some(ref reasoning) = choice.delta.reasoning_content {
                            accumulated_reasoning.push_str(reasoning);
                            _has_reasoning = true;
                        }
                        if let Some(ref tool_calls) = choice.delta.tool_calls {
                            for tc in tool_calls {
                                let idx = tc.index;
                                while accumulated_tool_calls.len() <= idx {
                                    accumulated_tool_calls.push((
                                        String::new(),
                                        String::new(),
                                        String::new(),
                                    ));
                                }
                                if let Some(ref id) = tc.id {
                                    accumulated_tool_calls[idx].0.clone_from(id);
                                }
                                if let Some(ref func) = tc.function {
                                    if let Some(ref name) = func.name {
                                        accumulated_tool_calls[idx].1.clone_from(name);
                                    }
                                    if let Some(ref args) = func.arguments {
                                        accumulated_tool_calls[idx].2.push_str(args);
                                    }
                                }
                            }
                        }
                    }
                } else {
                    // Pass through frames that don't parse as OpenAI chunks.
                    let sse_data = frame.to_sse_string();
                    if tx.send(Ok(Bytes::from(sse_data))).await.is_err() {
                        upstream_completed_cleanly = false;
                        break;
                    }
                    $forwarded = true;
                }
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
                                handle_openai_frame!(frame, forwarded);
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
                                handle_openai_frame!(frame, finish_forwarded);
                                let _ = finish_forwarded;
                            }
                            let _ = tx
                                .send(Err(std::io::Error::other(e.to_string())))
                                .await;
                            break;
                        }
                    }
                }
                _ = tx.closed() => {
                    tracing::debug!("Client disconnected, aborting upstream");
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
            handle_openai_frame!(frame, finish_forwarded);
            let _ = finish_forwarded;
        }

        if ended_with_timeout {
            send_stream_timeout_error(&tx, &tier_name, idle_timeout).await;
        }

        upstream_completed_cleanly &= saw_openai_terminal;

        // Send final stop events
        let mut usage = if input_tokens > 0 || output_tokens > 0 {
            Some(AnthropicUsage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens: None,
                reasoning_tokens,
            })
        } else {
            // Estimate from accumulated content if no usage reported
            let estimated_output = (accumulated_content.len() + accumulated_reasoning.len()) / 4;
            Some(AnthropicUsage {
                input_tokens,
                output_tokens: estimated_output as u64,
                cache_read_input_tokens,
                cache_creation_input_tokens: None,
                reasoning_tokens,
            })
        };

        if !chain.is_empty() {
            let mut content_blocks = Vec::new();
            if !accumulated_content.is_empty() {
                content_blocks.push(serde_json::json!({
                    "type": "text",
                    "text": accumulated_content
                }));
            }
            for (id, name, arguments) in &accumulated_tool_calls {
                let input = serde_json::from_str::<serde_json::Value>(arguments)
                    .unwrap_or_else(|_| serde_json::json!({"raw_arguments": arguments}));
                content_blocks.push(serde_json::json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": input
                }));
            }

            let resp_value = serde_json::json!({
                "content": content_blocks,
                "usage": {
                    "input_tokens": input_tokens,
                    "output_tokens": output_tokens
                }
            });

            // This aggregate exists only for tracing, so external plugins are
            // skipped: spawning a process to produce a discarded result costs
            // latency and the plugin never sees an aggregated stream document.
            if let Ok(transformed) = chain.apply_response_frame(resp_value) {
                trace!(transformed_response = %serde_json::to_string(&transformed).unwrap_or_default(),
                       "streaming response transformed");
            }
        }

        // OpenAI-compatible streams may omit the final usage chunk, leaving
        // input_tokens == 0 (translated Anthropic→OpenAI requests do not set
        // stream_options.include_usage). Fall back to the pre-request estimate
        // BEFORE serializing the stop events so the client-visible SSE usage
        // matches what /v1/usage and cost accounting record, mirroring
        // stream_anthropic_response_with_tracking. Drift verification below
        // deliberately keeps the raw upstream value.
        if let (Some(ctx), Some(usage)) = (&verify_ctx, usage.as_mut()) {
            if usage.input_tokens == 0 && ctx.local_estimate > 0 {
                usage.input_tokens = ctx.local_estimate;
            }
        }

        if ended_with_timeout {
            // An idle timeout is not a clean completion: close the open
            // content blocks and report the terminal, but do not record usage
            // or route success for a stream that never finished.
            send_translated_terminal_events(
                &tx,
                &mut translation_state,
                usage.clone().unwrap_or_default(),
            )
            .await;
            return;
        }

        send_translated_terminal_events(
            &tx,
            &mut translation_state,
            usage.clone().unwrap_or_default(),
        )
        .await;

        // Record usage and verify token drift if we have context
        if let Some(ctx) = &verify_ctx {
            if let Some(ref usage) = usage {
                // usage.input_tokens was normalized against the pre-request
                // estimate above. Drift verification receives the raw
                // upstream value instead: when the provider omitted usage
                // (raw 0) there is nothing to compare, and passing the
                // substituted estimate would record a false 0% drift sample.
                let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
                record_usage(
                    &ctx.tier_name,
                    usage.input_tokens,
                    usage.output_tokens,
                    cache_read,
                    0,
                );
                verify_token_usage(&ctx.tier_name, ctx.local_estimate, input_tokens);
                if let Some(cost) = ctx.pricing.and_then(|p| {
                    // OpenAI-style prompt_tokens already include the cached
                    // share; bill that share at the cached rate when one is
                    // configured (zero otherwise, keeping a lower bound).
                    p.estimate_request_cost_usd_with_cache(
                        usage.input_tokens.saturating_sub(cache_read),
                        cache_read,
                        0,
                        usage.output_tokens,
                    )
                }) {
                    record_cost(&ctx.tier_name, cost);
                }
            }

            // Record TTFT and throughput metrics
            let final_output = usage.as_ref().map(|u| u.output_tokens).unwrap_or(0);
            let ttft_secs = first_token_time
                .map(|ft| ft.duration_since(stream_start.unwrap_or(ft)).as_secs_f64())
                .unwrap_or(0.0);
            if ttft_secs > 0.0 {
                record_ttft(&ctx.tier_name, ttft_secs);
            }
            if let (Some(ft), Some(lt)) = (first_token_time, last_token_time) {
                let gen_secs = lt.duration_since(ft).as_secs_f64();
                if gen_secs > 0.0 && final_output > 0 {
                    record_throughput(&ctx.tier_name, final_output, gen_secs, ttft_secs);
                }
            }

            // Clear rate limit backoff and update rate limit state on successful stream completion.
            // A mid-stream upstream error or client disconnect is not a success signal for the route.
            if upstream_completed_cleanly {
                if let Some(permit) = ctx.admission_permit.as_ref() {
                    permit.record_success();
                }
                if let Some(ref tracker) = ctx.ratelimit_tracker {
                    if let Some((remaining, reset_at)) = &ctx.rate_limit_info {
                        tracker.record_success_if_current(
                            &ctx.ratelimit_key,
                            ctx.ratelimit_generation,
                            *remaining,
                            *reset_at,
                        );
                    } else {
                        tracker.record_success_if_current(
                            &ctx.ratelimit_key,
                            ctx.ratelimit_generation,
                            None,
                            None,
                        );
                    }
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
