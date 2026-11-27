use futures::StreamExt;

use super::*;
use crate::transformer::TransformerChain;

#[tokio::test]
async fn translated_live_stream_preserves_usage_token_details() {
    let first = serde_json::json!({
        "id": "chatcmpl-usage-details",
        "object": "chat.completion.chunk",
        "created": 42,
        "model": "test-model",
        "choices": [{
            "index": 0,
            "delta": {"content": "ok"},
            "finish_reason": "stop"
        }]
    });
    let usage = serde_json::json!({
        "id": "chatcmpl-usage-details",
        "object": "chat.completion.chunk",
        "created": 42,
        "model": "test-model",
        "choices": [],
        "usage": {
            "prompt_tokens": 12,
            "completion_tokens": 7,
            "total_tokens": 19,
            "prompt_tokens_details": {"cached_tokens": 5},
            "completion_tokens_details": {"reasoning_tokens": 3}
        }
    });
    let payload = format!("data: {first}\n\ndata: {usage}\n\ndata: [DONE]\n\n");
    let byte_stream: BoxByteStream =
        Box::pin(futures::stream::iter([Ok::<Bytes, reqwest::Error>(
            Bytes::from(payload),
        )]));

    let response =
        stream_response_translated(byte_stream, 8, None, "test-model", TransformerChain::new())
            .await;
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    let usage = body
        .split("\n\n")
        .filter_map(|frame| {
            frame
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .and_then(|data| serde_json::from_str::<serde_json::Value>(data).ok())
        })
        .find(|event| event["type"] == "message_delta")
        .and_then(|event| event.get("usage").cloned())
        .expect("message_delta should carry structured usage");

    assert_eq!(usage["input_tokens"], 12);
    assert_eq!(usage["output_tokens"], 7);
    assert_eq!(usage["cache_read_input_tokens"], 5);
    assert_eq!(usage["reasoning_tokens"], 3);
}

fn tracked_context(
    tracker: &std::sync::Arc<crate::ratelimit::RateLimitTracker>,
) -> (
    crate::sse::StreamVerifyCtx,
    std::sync::Arc<crate::admission::AdmissionTracker>,
) {
    let admission = std::sync::Arc::new(crate::admission::AdmissionTracker::new());
    let permit = admission
        .acquire("provider,model", Some(8), 1, None)
        .expect("admission fixture");
    let context = crate::sse::StreamVerifyCtx {
        tier_name: "provider".to_string(),
        ratelimit_key: "provider,model".to_string(),
        ratelimit_generation: tracker.generation("provider,model"),
        local_estimate: 1,
        ratelimit_tracker: Some(tracker.clone()),
        admission_permit: Some(permit),
        rate_limit_info: None,
        stream_start: std::time::Instant::now(),
        stream_idle_timeout: std::time::Duration::from_secs(5),
        pricing: None,
    };
    (context, admission)
}

#[tokio::test]
async fn truncated_openai_stream_is_not_a_route_success() {
    let tracker = std::sync::Arc::new(crate::ratelimit::RateLimitTracker::new());
    tracker.record_429("provider,model", Some(std::time::Duration::from_secs(30)));
    let (context, _admission) = tracked_context(&tracker);
    let payload = r#"data: {"id":"partial","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":null}]}
"#;
    let byte_stream: BoxByteStream =
        Box::pin(futures::stream::iter([Ok::<Bytes, reqwest::Error>(
            Bytes::from(payload),
        )]));

    let response = stream_response_translated(
        byte_stream,
        8,
        Some(context),
        "model",
        TransformerChain::new(),
    )
    .await;
    let _ = axum::body::to_bytes(response.into_body(), 1024 * 1024).await;

    assert!(tracker.has_backoff("provider,model"));
}

#[tokio::test]
async fn terminal_openai_stream_clears_route_backoff() {
    let tracker = std::sync::Arc::new(crate::ratelimit::RateLimitTracker::new());
    tracker.record_429("provider,model", Some(std::time::Duration::from_secs(30)));
    let (context, _admission) = tracked_context(&tracker);
    let payload = r#"data: {"id":"done","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: [DONE]

"#;
    let byte_stream: BoxByteStream =
        Box::pin(futures::stream::iter([Ok::<Bytes, reqwest::Error>(
            Bytes::from(payload),
        )]));

    let response = stream_response_translated(
        byte_stream,
        8,
        Some(context),
        "model",
        TransformerChain::new(),
    )
    .await;
    let _ = axum::body::to_bytes(response.into_body(), 1024 * 1024).await;

    assert!(!tracker.has_backoff("provider,model"));
}

#[tokio::test]
async fn truncated_anthropic_stream_is_not_a_route_success() {
    let tracker = std::sync::Arc::new(crate::ratelimit::RateLimitTracker::new());
    tracker.record_429("provider,model", Some(std::time::Duration::from_secs(30)));
    let (context, _admission) = tracked_context(&tracker);
    let payload = "event: message_start\ndata: {\"type\":\"message_start\"}\n\n";
    let byte_stream: BoxByteStream =
        Box::pin(futures::stream::iter([Ok::<Bytes, reqwest::Error>(
            Bytes::from(payload),
        )]));

    let response =
        stream_anthropic_response_with_tracking(byte_stream, 8, context, TransformerChain::new())
            .await;
    let _ = axum::body::to_bytes(response.into_body(), 1024 * 1024).await;

    assert!(tracker.has_backoff("provider,model"));
}

#[tokio::test]
async fn terminal_anthropic_stream_clears_route_backoff() {
    let tracker = std::sync::Arc::new(crate::ratelimit::RateLimitTracker::new());
    tracker.record_429("provider,model", Some(std::time::Duration::from_secs(30)));
    let (context, _admission) = tracked_context(&tracker);
    let payload = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let byte_stream: BoxByteStream =
        Box::pin(futures::stream::iter([Ok::<Bytes, reqwest::Error>(
            Bytes::from(payload),
        )]));

    let response =
        stream_anthropic_response_with_tracking(byte_stream, 8, context, TransformerChain::new())
            .await;
    let _ = axum::body::to_bytes(response.into_body(), 1024 * 1024).await;

    assert!(!tracker.has_backoff("provider,model"));
}

#[tokio::test]
async fn flush_recovers_final_openai_tool_call_without_blank_line() {
    let final_chunk = serde_json::json!({
        "id": "chatcmpl-flush",
        "object": "chat.completion.chunk",
        "created": 1,
        "model": "m",
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_flush",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": "{\"city\":\"Paris\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });
    // The data line is complete, but the blank-line separator never arrives.
    let payload = format!("data: {final_chunk}\n");
    let byte_stream: BoxByteStream =
        Box::pin(futures::stream::iter([Ok::<Bytes, reqwest::Error>(
            Bytes::from(payload),
        )]));

    let response =
        stream_response_translated(byte_stream, 8, None, "model", TransformerChain::new()).await;
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    let events: Vec<serde_json::Value> = body
        .split("\n\n")
        .filter_map(|frame| {
            frame
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .and_then(|data| serde_json::from_str::<serde_json::Value>(data).ok())
        })
        .collect();

    let partial_json = events
        .iter()
        .find_map(|event| {
            event
                .get("delta")
                .and_then(|delta| delta.get("partial_json"))
                .and_then(|value| value.as_str())
        })
        .expect("flushed tool-call delta should be forwarded");
    assert_eq!(partial_json, "{\"city\":\"Paris\"}");

    let stop_reason = events
        .iter()
        .find(|event| event["type"] == "message_delta")
        .and_then(|event| event["delta"]["stop_reason"].as_str())
        .expect("flushed finish_reason should set stop_reason");
    assert_eq!(stop_reason, "tool_use");
}

#[tokio::test]
async fn flush_recovers_final_anthropic_tool_call_without_blank_line() {
    // The terminal message_delta is followed by the final tool-call delta,
    // whose blank-line separator never arrives.
    let payload = concat!(
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},",
        "\"usage\":{\"output_tokens\":7}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,",
        "\"delta\":{\"type\":\"input_json_delta\",",
        "\"partial_json\":\"{\\\"city\\\":\\\"Paris\\\"}\"}}\n"
    );
    let byte_stream: BoxByteStream =
        Box::pin(futures::stream::iter([Ok::<Bytes, reqwest::Error>(
            Bytes::from(payload),
        )]));

    let response = stream_anthropic_response_with_tracking(
        byte_stream,
        8,
        tracked_context(&std::sync::Arc::new(
            crate::ratelimit::RateLimitTracker::new(),
        ))
        .0,
        TransformerChain::new(),
    )
    .await;
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    let events: Vec<serde_json::Value> = body
        .split("\n\n")
        .filter_map(|frame| {
            frame
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .and_then(|data| serde_json::from_str::<serde_json::Value>(data).ok())
        })
        .collect();

    let partial_json = events
        .iter()
        .find_map(|event| {
            event
                .get("delta")
                .and_then(|delta| delta.get("partial_json"))
                .and_then(|value| value.as_str())
        })
        .expect("flushed tool-call delta should be forwarded");
    assert_eq!(partial_json, "{\"city\":\"Paris\"}");

    let stop_reason = events
        .iter()
        .find(|event| event["type"] == "message_delta")
        .and_then(|event| event["delta"]["stop_reason"].as_str())
        .expect("preserved terminal frame should carry stop_reason");
    assert_eq!(stop_reason, "tool_use");
}

#[tokio::test]
async fn json_to_sse_rewrite_removes_stale_entity_headers() {
    let payload = serde_json::json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": "ok"}],
        "model": "test-model",
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    });
    let mut response = Response::new(Body::from(payload.to_string()));
    for (name, value) in [
        (axum::http::header::CONTENT_LENGTH, "10"),
        (axum::http::header::CONTENT_ENCODING, "gzip"),
        (axum::http::header::TRANSFER_ENCODING, "chunked"),
        (axum::http::header::ETAG, "\"old\""),
        (
            axum::http::header::LAST_MODIFIED,
            "Fri, 01 Aug 2025 00:00:00 GMT",
        ),
    ] {
        response
            .headers_mut()
            .insert(name, axum::http::HeaderValue::from_str(value).unwrap());
    }

    let rewritten = wrap_json_response_as_sse(response, false).await;

    assert_eq!(
        rewritten.headers().get(axum::http::header::CONTENT_TYPE),
        Some(&axum::http::HeaderValue::from_static("text/event-stream"))
    );
    for name in [
        axum::http::header::CONTENT_LENGTH,
        axum::http::header::CONTENT_ENCODING,
        axum::http::header::TRANSFER_ENCODING,
        axum::http::header::ETAG,
        axum::http::header::LAST_MODIFIED,
    ] {
        assert!(rewritten.headers().get(name).is_none());
    }
}

#[test]
fn test_emit_anthropic_sse_events_tool_use() {
    let resp = AnthropicResponse {
        id: "msg_test".to_string(),
        response_type: "message".to_string(),
        role: "assistant".to_string(),
        content: vec![
            AnthropicContentBlock::Text {
                text: " ".to_string(),
            },
            AnthropicContentBlock::ToolUse {
                id: "toolu_123".to_string(),
                name: "Read".to_string(),
                input: serde_json::json!({"file_path": "/tmp/test.py"}),
            },
        ],
        model: "kimi-k2.5".to_string(),
        stop_reason: Some("tool_use".to_string()),
        usage: AnthropicUsage {
            input_tokens: 100,
            output_tokens: 20,
            ..Default::default()
        },
        reasoning_content: None,
        refusal: None,
        response_status: None,
        incomplete_details: None,
    };

    let events = emit_anthropic_sse_events(&resp, true);
    let joined = events.join("");

    // Must contain message_start
    assert!(joined.contains("message_start"), "missing message_start");
    // Must contain the text block
    assert!(joined.contains("text_delta"), "missing text_delta");
    // Must contain tool_use content_block_start
    assert!(
        joined.contains("\"type\":\"tool_use\""),
        "missing tool_use block"
    );
    assert!(joined.contains("\"name\":\"Read\""), "missing tool name");
    assert!(joined.contains("\"id\":\"toolu_123\""), "missing tool id");
    // Must contain tool input as input_json_delta
    assert!(
        joined.contains("input_json_delta"),
        "missing input_json_delta"
    );
    assert!(joined.contains("file_path"), "missing tool input content");
    // Must contain stop_reason
    assert!(
        joined.contains("\"stop_reason\":\"tool_use\""),
        "missing stop_reason"
    );
    // Must have message_stop
    assert!(joined.contains("message_stop"), "missing message_stop");
}

#[test]
fn test_emit_anthropic_sse_events_cache_usage() {
    let resp = AnthropicResponse {
        id: "msg_test".to_string(),
        response_type: "message".to_string(),
        role: "assistant".to_string(),
        content: vec![AnthropicContentBlock::Text {
            text: "ok".to_string(),
        }],
        model: "test-model".to_string(),
        stop_reason: Some("end_turn".to_string()),
        usage: AnthropicUsage {
            input_tokens: 100,
            output_tokens: 20,
            cache_read_input_tokens: Some(60),
            cache_creation_input_tokens: Some(30),
            ..Default::default()
        },
        reasoning_content: None,
        refusal: None,
        response_status: None,
        incomplete_details: None,
    };

    let events = emit_anthropic_sse_events(&resp, true);
    let start_usage = events
        .iter()
        .filter_map(|event| event.lines().find_map(|line| line.strip_prefix("data: ")))
        .filter_map(|data| serde_json::from_str::<serde_json::Value>(data).ok())
        .find(|event| event["type"] == "message_start")
        .map(|event| event["message"]["usage"].clone())
        .expect("message_start should carry usage");

    assert_eq!(start_usage["input_tokens"], 100);
    assert_eq!(start_usage["cache_read_input_tokens"], 60);
    assert_eq!(start_usage["cache_creation_input_tokens"], 30);
}

#[test]
fn test_emit_anthropic_sse_events_thinking() {
    let resp = AnthropicResponse {
        id: "msg_think".to_string(),
        response_type: "message".to_string(),
        role: "assistant".to_string(),
        content: vec![
            AnthropicContentBlock::Thinking {
                thinking: "Let me analyze this...".to_string(),
                signature: "sig123".to_string(),
            },
            AnthropicContentBlock::Text {
                text: "Here is my answer.".to_string(),
            },
        ],
        model: "kimi-k2.5".to_string(),
        stop_reason: Some("end_turn".to_string()),
        usage: AnthropicUsage {
            input_tokens: 50,
            output_tokens: 30,
            cache_read_input_tokens: Some(7),
            cache_creation_input_tokens: None,
            reasoning_tokens: Some(11),
        },
        reasoning_content: None,
        refusal: None,
        response_status: None,
        incomplete_details: None,
    };

    let events = emit_anthropic_sse_events(&resp, true);
    let joined = events.join("");

    assert!(joined.contains("thinking_delta"), "missing thinking_delta");
    assert!(
        joined.contains("Let me analyze this..."),
        "missing thinking content"
    );
    assert!(
        joined.contains("signature_delta"),
        "missing signature_delta"
    );
    assert!(joined.contains("sig123"), "missing signature content");
    assert!(joined.contains("text_delta"), "missing text_delta");
    assert!(joined.contains("\"cache_read_input_tokens\":7"));
    assert!(joined.contains("\"reasoning_tokens\":11"));
}

#[test]
fn unsigned_reasoning_is_only_emitted_for_openai_adapters() {
    let resp = AnthropicResponse {
        id: "msg_unsigned".to_string(),
        response_type: "message".to_string(),
        role: "assistant".to_string(),
        content: vec![AnthropicContentBlock::Text {
            text: "answer".to_string(),
        }],
        model: "responses-model".to_string(),
        stop_reason: Some("end_turn".to_string()),
        usage: AnthropicUsage {
            input_tokens: 3,
            output_tokens: 2,
            ..Default::default()
        },
        reasoning_content: Some("unsigned summary".to_string()),
        refusal: None,
        response_status: None,
        incomplete_details: None,
    };

    let native_events = emit_anthropic_sse_events(&resp, false).join("");
    assert!(!native_events.contains("thinking_delta"));
    assert!(!native_events.contains("unsigned summary"));

    let adapter_events = emit_anthropic_sse_events(&resp, true).join("");
    assert!(adapter_events.contains("thinking_delta"));
    assert!(adapter_events.contains("unsigned summary"));

    let whitespace_resp = AnthropicResponse {
        reasoning_content: Some("  \n".to_string()),
        ..resp
    };
    let whitespace_events = emit_anthropic_sse_events(&whitespace_resp, true).join("");
    assert!(!whitespace_events.contains("thinking_delta"));
}

#[test]
fn pseudo_stream_omits_absent_responses_metadata() {
    let resp = AnthropicResponse {
        id: "msg_plain".to_string(),
        response_type: "message".to_string(),
        role: "assistant".to_string(),
        content: vec![],
        model: "plain-model".to_string(),
        stop_reason: Some("end_turn".to_string()),
        usage: AnthropicUsage::default(),
        reasoning_content: None,
        refusal: None,
        response_status: None,
        incomplete_details: None,
    };

    let first = emit_anthropic_sse_events(&resp, false)
        .into_iter()
        .next()
        .unwrap();
    let payload = first.split_once("data: ").unwrap().1.trim();
    let event: serde_json::Value = serde_json::from_str(payload).unwrap();
    let message = event["message"].as_object().unwrap();

    assert!(!message.contains_key("refusal"));
    assert!(!message.contains_key("response_status"));
    assert!(!message.contains_key("incomplete_details"));
}

#[test]
fn pseudo_stream_carries_responses_refusal_and_terminal_metadata() {
    let resp = AnthropicResponse {
        id: "msg_refusal".to_string(),
        response_type: "message".to_string(),
        role: "assistant".to_string(),
        content: vec![],
        model: "responses-model".to_string(),
        stop_reason: Some("stop_sequence".to_string()),
        usage: AnthropicUsage::default(),
        reasoning_content: None,
        refusal: Some("I cannot help with that.".to_string()),
        response_status: Some("incomplete".to_string()),
        incomplete_details: Some(serde_json::json!({"reason": "content_filter"})),
    };

    let events = emit_anthropic_sse_events(&resp, true).join("");

    assert!(events.contains("I cannot help with that."));
    assert!(events.contains("\"response_status\":\"incomplete\""));
    assert!(events.contains("\"reason\":\"content_filter\""));
}

#[test]
fn test_emit_anthropic_sse_events_multiple_tools() {
    let resp = AnthropicResponse {
        id: "msg_multi".to_string(),
        response_type: "message".to_string(),
        role: "assistant".to_string(),
        content: vec![
            AnthropicContentBlock::Text {
                text: "I'll read both files.".to_string(),
            },
            AnthropicContentBlock::ToolUse {
                id: "toolu_1".to_string(),
                name: "Read".to_string(),
                input: serde_json::json!({"file_path": "/a.py"}),
            },
            AnthropicContentBlock::ToolUse {
                id: "toolu_2".to_string(),
                name: "Read".to_string(),
                input: serde_json::json!({"file_path": "/b.py"}),
            },
        ],
        model: "kimi-k2.5".to_string(),
        stop_reason: Some("tool_use".to_string()),
        usage: AnthropicUsage {
            input_tokens: 100,
            output_tokens: 40,
            ..Default::default()
        },
        reasoning_content: None,
        refusal: None,
        response_status: None,
        incomplete_details: None,
    };

    let events = emit_anthropic_sse_events(&resp, true);
    let joined = events.join("");

    // Both tool IDs must be present
    assert!(joined.contains("toolu_1"), "missing first tool id");
    assert!(joined.contains("toolu_2"), "missing second tool id");
    // Both file paths must be in the input
    assert!(joined.contains("/a.py"), "missing first tool input");
    assert!(joined.contains("/b.py"), "missing second tool input");
    // Three content_block_stop SSE events (text + 2 tools)
    // Each produces "event: content_block_stop\ndata: {\"type\":\"content_block_stop\"...}"
    // so the event line appears 3 times
    assert_eq!(
        joined.matches("event: content_block_stop").count(),
        3,
        "expected 3 content_block_stop events"
    );
}

fn context_with_idle(
    tracker: &std::sync::Arc<crate::ratelimit::RateLimitTracker>,
    idle: std::time::Duration,
) -> crate::sse::StreamVerifyCtx {
    let (context, _admission) = tracked_context(tracker);
    crate::sse::StreamVerifyCtx {
        stream_idle_timeout: idle,
        ..context
    }
}

/// A stream that delivers one chunk and then goes silent forever.
fn hanging_stream(payload: &'static str) -> BoxByteStream {
    Box::pin(futures::stream::unfold(0, move |state| async move {
        match state {
            0 => Some((Ok::<Bytes, reqwest::Error>(Bytes::from(payload)), 1)),
            _ => {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                None
            }
        }
    }))
}

#[tokio::test]
async fn idle_timeout_flushes_the_pending_final_frame() {
    // The upstream sends a complete data line but never the blank-line
    // separator, then goes silent. `push` only releases blank-line
    // terminated frames, so the idle-timeout branch must flush the
    // pending tail instead of dropping it.
    let payload = "data: {\"id\":\"idle-flush\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"tail\"},\"finish_reason\":null}]}\n";
    let tracker = std::sync::Arc::new(crate::ratelimit::RateLimitTracker::new());
    let context = context_with_idle(&tracker, std::time::Duration::from_millis(50));

    let response = stream_response_translated(
        hanging_stream(payload),
        8,
        Some(context),
        "model",
        TransformerChain::new(),
    )
    .await;
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();

    assert!(
        body.contains("tail"),
        "the pending final frame must be flushed on idle timeout: {body}"
    );
    assert!(
        body.contains("timeout_error"),
        "the idle timeout must still be reported: {body}"
    );
    assert!(
        body.contains("\"type\":\"content_block_stop\""),
        "open content blocks must be closed on idle timeout: {body}"
    );
}

#[tokio::test]
async fn anthropic_idle_timeout_flushes_the_pending_final_frame() {
    let payload = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"tail\\\":true}\"}}\n";
    let tracker = std::sync::Arc::new(crate::ratelimit::RateLimitTracker::new());
    let context = context_with_idle(&tracker, std::time::Duration::from_millis(50));

    let response = stream_anthropic_response_with_tracking(
        hanging_stream(payload),
        8,
        context,
        TransformerChain::new(),
    )
    .await;
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();

    assert!(
        body.contains("tail"),
        "the pending final frame must be flushed on idle timeout: {body}"
    );
    assert!(
        body.contains("timeout_error"),
        "the idle timeout must still be reported: {body}"
    );
}

#[tokio::test]
async fn upstream_read_error_flushes_the_pending_frame_before_the_error() {
    // The upstream sends a complete data line without the blank-line
    // separator and then fails. The pending tail must reach the client
    // before the transport error.
    let payload = "data: {\"id\":\"read-error\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"tail\"},\"finish_reason\":null}]}\n";
    let error = reqwest::Client::builder()
        .build()
        .expect("http client")
        .get("http://[::1")
        .send()
        .await
        .expect_err("an unparsable URL fails without touching the network");
    let byte_stream: BoxByteStream = Box::pin(futures::stream::iter([
        Ok(Bytes::from(payload)),
        Err(error),
    ]));

    let response =
        stream_response_translated(byte_stream, 8, None, "model", TransformerChain::new()).await;
    let mut body = String::new();
    let mut saw_error = false;
    {
        let mut stream = response.into_body().into_data_stream();
        while let Some(item) = stream.next().await {
            match item {
                Ok(bytes) => body.push_str(&String::from_utf8_lossy(&bytes)),
                Err(_) => {
                    saw_error = true;
                    break;
                }
            }
        }
    }

    assert!(saw_error, "the transport error must still reach the client");
    assert!(
        body.contains("tail"),
        "the pending final frame must be flushed before the transport error: {body}"
    );
    assert!(
        !body.contains("event: message_stop"),
        "no successful terminal after a transport error: {body}"
    );
}

#[tokio::test]
async fn a_stream_without_a_finish_reason_closes_blocks_and_reports_truncated_arguments() {
    // The upstream ends after a truncated tool argument and never sends a
    // finish reason.
    let payload = concat!(
            "data: {\"id\":\"trunc\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",",
            "\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"writing\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"trunc\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",",
            "\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_trunc\",\"type\":\"function\",",
            "\"function\":{\"name\":\"write_file\",\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\",\\\"content\\\":\\\"abc\"}}]},\"finish_reason\":null}]}\n\n"
        );
    let byte_stream: BoxByteStream =
        Box::pin(futures::stream::iter([Ok::<Bytes, reqwest::Error>(
            Bytes::from(payload),
        )]));

    let response =
        stream_response_translated(byte_stream, 8, None, "model", TransformerChain::new()).await;
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    let events: Vec<serde_json::Value> = body
        .split("\n\n")
        .filter_map(|frame| {
            frame
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .and_then(|data| serde_json::from_str::<serde_json::Value>(data).ok())
        })
        .collect();

    // Every started block is closed exactly once.
    let stops: Vec<u64> = events
        .iter()
        .filter(|event| event["type"] == "content_block_stop")
        .filter_map(|event| event["index"].as_u64())
        .collect();
    assert_eq!(stops, vec![0, 1]);

    // The message never saw a finish reason, so it reports no stop reason.
    let message_delta = events
        .iter()
        .find(|event| event["type"] == "message_delta")
        .expect("message_delta");
    assert_eq!(
        message_delta["delta"]["stop_reason"],
        serde_json::Value::Null
    );

    // The truncated arguments are reported before the terminal
    // message_stop, so the client never reads a clean success.
    let error_position = events
        .iter()
        .position(|event| event["type"] == "error")
        .expect("truncated tool arguments must be reported");
    let stop_position = events
        .iter()
        .position(|event| event["type"] == "message_stop")
        .expect("terminal message_stop");
    assert!(
        error_position < stop_position,
        "the truncation error must precede the terminal message_stop"
    );
    assert!(
        events.iter().all(
            |event| event["type"] != "message_delta" || event["delta"]["stop_reason"].is_null()
        ),
        "no event may report a clean stop reason"
    );
    let error = &events[error_position];
    assert_eq!(error["error"]["type"], "invalid_request_error");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("truncated"),
        "the error must describe the truncation: {error}"
    );
}
