// SPDX-License-Identifier: AGPL-3.0-or-later
use anthropic::emit_anthropic_sse_events;
use axum::body::Body;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use super::types::*;

use bytes::Bytes;

mod anthropic;
mod openai_translated;

pub use anthropic::stream_anthropic_response_with_tracking;
pub use openai_translated::stream_response_translated;

#[cfg(test)]
mod tests;

async fn send_stream_timeout_error(
    tx: &tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
    tier_name: &str,
    idle_timeout: std::time::Duration,
) {
    let message = format!(
        "CCR stream idle timeout on {} after {}ms",
        tier_name,
        idle_timeout.as_millis()
    );
    let payload = serde_json::json!({
        "type": "error",
        "error": {
            "type": "timeout_error",
            "message": message
        }
    });
    let sse_data = format!("event: error\ndata: {}\n\n", payload);
    let _ = tx.send(Ok(Bytes::from(sse_data))).await;
}

/// A boxed byte stream that both streaming handlers accept.
pub type BoxByteStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

/// Wrap a non-streaming Anthropic JSON response as pseudo-SSE.
///
/// Used when `forceNonStreaming` is true but the client requested `stream: true`.
/// Reads the response body, parses it as an `AnthropicResponse`, and re-emits it
/// as SSE events that Claude CLI can parse. Falls through to the original response
/// if parsing fails.
pub(super) async fn wrap_json_response_as_sse(
    response: Response,
    include_unsigned_reasoning: bool,
) -> Response {
    let (mut parts, body) = response.into_parts();

    // Only wrap successful JSON responses
    if parts.status != StatusCode::OK {
        return Response::from_parts(parts, body);
    }

    let bytes = match axum::body::to_bytes(body, 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to read response body",
            )
                .into_response();
        }
    };

    // Try to parse as AnthropicResponse
    if let Ok(anthropic_resp) = serde_json::from_slice::<AnthropicResponse>(&bytes) {
        let sse_events = emit_anthropic_sse_events(&anthropic_resp, include_unsigned_reasoning);
        let sse_body = sse_events.join("");
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
        Response::from_parts(parts, Body::from(sse_body))
    } else {
        // Can't parse as Anthropic — return original response unchanged
        Response::from_parts(parts, Body::from(bytes))
    }
}
