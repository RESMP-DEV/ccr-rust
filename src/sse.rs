use axum::{
    body::Body,
    http::{header, StatusCode},
    response::Response,
};
use bytes::Bytes;
use futures::StreamExt;
use serde::Deserialize;
use tokio_stream::wrappers::ReceiverStream;

use crate::metrics::{
    increment_active_streams, record_stream_backpressure, record_usage, verify_token_usage,
};

/// Usage block extracted from an SSE `message_delta` or `message_stop` event.
#[derive(Debug, Deserialize, Default)]
struct StreamUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

/// Wrapper for extracting the `usage` field from an SSE event's JSON data.
#[derive(Debug, Deserialize)]
struct StreamEventWithUsage {
    #[serde(default)]
    usage: Option<StreamUsage>,
}

/// Attempt to extract usage from a chunk of SSE data. SSE chunks may contain
/// multiple `data:` lines; we scan each one for a JSON object with a `usage`
/// field. Returns the last usage block found (the final event in a stream
/// typically carries the cumulative totals).
fn extract_usage_from_sse_chunk(chunk: &[u8]) -> Option<StreamUsage> {
    let text = std::str::from_utf8(chunk).ok()?;
    let mut last_usage: Option<StreamUsage> = None;

    for line in text.lines() {
        let data = line.strip_prefix("data:").or_else(|| line.strip_prefix("data: "));
        if let Some(json_str) = data {
            let json_str = json_str.trim();
            if json_str == "[DONE]" || json_str.is_empty() {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<StreamEventWithUsage>(json_str) {
                if event.usage.is_some() {
                    last_usage = event.usage;
                }
            }
        }
    }

    last_usage
}

/// Context for token verification on streaming responses.
pub struct StreamVerifyCtx {
    pub tier_name: String,
    pub local_estimate: u64,
}

pub async fn stream_response(
    resp: reqwest::Response,
    buffer_size: usize,
    verify_ctx: Option<StreamVerifyCtx>,
) -> Response {
    increment_active_streams(1);

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(buffer_size);

    tokio::spawn(async move {
        let mut stream = resp.bytes_stream();
        let mut last_usage: Option<StreamUsage> = None;

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    // Scan for usage data in SSE events before forwarding.
                    if verify_ctx.is_some() {
                        if let Some(usage) = extract_usage_from_sse_chunk(&bytes) {
                            last_usage = Some(usage);
                        }
                    }

                    // Capacity check before blocking send gives us a backpressure signal.
                    if tx.capacity() == 0 {
                        record_stream_backpressure();
                    }
                    if tx.send(Ok(bytes)).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx
                        .send(Err(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            e.to_string(),
                        )))
                        .await;
                    break;
                }
            }
        }

        // Stream finished: record usage and verify token drift if we have context.
        if let Some(ctx) = &verify_ctx {
            if let Some(usage) = last_usage {
                record_usage(
                    &ctx.tier_name,
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.cache_read_input_tokens,
                    usage.cache_creation_input_tokens,
                );
                verify_token_usage(&ctx.tier_name, ctx.local_estimate, usage.input_tokens);
            }
        }

        increment_active_streams(-1);
    });

    let body = Body::from_stream(ReceiverStream::new(rx));

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(body)
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_usage_from_message_delta() {
        let chunk = b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"input_tokens\":150,\"output_tokens\":42}}\n\n";
        let usage = extract_usage_from_sse_chunk(chunk).unwrap();
        assert_eq!(usage.input_tokens, 150);
        assert_eq!(usage.output_tokens, 42);
    }

    #[test]
    fn extract_usage_with_cache_fields() {
        let chunk = b"data: {\"type\":\"message_delta\",\"usage\":{\"input_tokens\":500,\"output_tokens\":100,\"cache_read_input_tokens\":200,\"cache_creation_input_tokens\":50}}\n\n";
        let usage = extract_usage_from_sse_chunk(chunk).unwrap();
        assert_eq!(usage.input_tokens, 500);
        assert_eq!(usage.output_tokens, 100);
        assert_eq!(usage.cache_read_input_tokens, 200);
        assert_eq!(usage.cache_creation_input_tokens, 50);
    }

    #[test]
    fn no_usage_in_content_block() {
        let chunk = b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n";
        assert!(extract_usage_from_sse_chunk(chunk).is_none());
    }

    #[test]
    fn done_event_ignored() {
        let chunk = b"data: [DONE]\n\n";
        assert!(extract_usage_from_sse_chunk(chunk).is_none());
    }

    #[test]
    fn multiple_events_returns_last_usage() {
        let chunk = b"data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"hi\"}}\ndata: {\"type\":\"message_delta\",\"usage\":{\"input_tokens\":300,\"output_tokens\":75}}\n\n";
        let usage = extract_usage_from_sse_chunk(chunk).unwrap();
        assert_eq!(usage.input_tokens, 300);
        assert_eq!(usage.output_tokens, 75);
    }

    #[test]
    fn empty_chunk_returns_none() {
        assert!(extract_usage_from_sse_chunk(b"").is_none());
    }

    #[test]
    fn invalid_utf8_returns_none() {
        assert!(extract_usage_from_sse_chunk(&[0xff, 0xfe, 0xfd]).is_none());
    }

    #[test]
    fn data_prefix_without_space() {
        let chunk = b"data:{\"usage\":{\"input_tokens\":42,\"output_tokens\":10}}\n\n";
        let usage = extract_usage_from_sse_chunk(chunk).unwrap();
        assert_eq!(usage.input_tokens, 42);
    }
}
