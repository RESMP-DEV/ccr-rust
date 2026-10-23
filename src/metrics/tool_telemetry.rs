use axum::body::Body;
use axum::http::header::CONTENT_TYPE;
use axum::response::Response;
use bytes::Bytes;
use futures::StreamExt;
use prometheus::{register_counter_vec, register_histogram_vec, CounterVec, HistogramVec};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use tokio_stream::wrappers::ReceiverStream;

use crate::frontend::FrontendType;
use crate::router::{AnthropicContentBlock, AnthropicResponse};
use crate::sse::SseFrameDecoder;

use super::persistence::{persist_counter_inc, persist_histogram_observe};

pub(crate) const TOOL_CALLS_TOTAL: &str = "ccr_tool_calls_total";
pub(crate) const TOOL_DEFINITION_BYTES: &str = "ccr_tool_definition_bytes";
pub(crate) const TOOL_ARGUMENT_BYTES: &str = "ccr_tool_argument_bytes";
pub(crate) const TOOL_RESULT_BYTES: &str = "ccr_tool_result_bytes";
pub(crate) const TOOL_SIZE_BUCKETS: &[f64] = &[
    128.0, 512.0, 1024.0, 2048.0, 4096.0, 8192.0, 16384.0, 32768.0, 65536.0,
];
pub(crate) const TOOL_RESULT_BUCKETS: &[f64] = &[
    1024.0, 4096.0, 8192.0, 16384.0, 32768.0, 65536.0, 131072.0, 262144.0, 524288.0,
];

const MAX_TOOL_LABEL_CARDINALITY: usize = 128;
const MAX_SEEN_TOOL_EVENTS: usize = 8192;
const MAX_TELEMETRY_JSON_BYTES: usize = 16 * 1024 * 1024;
const MAX_DEDUP_ID_CHARS: usize = 256;

#[derive(Default)]
struct SeenToolEvents {
    seen: HashSet<String>,
    order: VecDeque<String>,
}

impl SeenToolEvents {
    fn should_record(&mut self, key: String) -> bool {
        if self.seen.contains(&key) {
            return false;
        }
        if self.seen.len() >= MAX_SEEN_TOOL_EVENTS {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        self.seen.insert(key.clone());
        self.order.push_back(key);
        true
    }
}

lazy_static::lazy_static! {
    pub(crate) static ref CALLS: CounterVec = register_counter_vec!(
        TOOL_CALLS_TOTAL,
        "Tool calls and tool results observed by CCR",
        &["frontend", "tool", "direction"]
    )
    .unwrap();

    pub(crate) static ref DEFINITION_BYTES: HistogramVec = register_histogram_vec!(
        TOOL_DEFINITION_BYTES,
        "Serialized tool definition bytes repeatedly sent to models",
        &["frontend", "tool"],
        TOOL_SIZE_BUCKETS.to_vec()
    )
    .unwrap();

    pub(crate) static ref ARGUMENT_BYTES: HistogramVec = register_histogram_vec!(
        TOOL_ARGUMENT_BYTES,
        "Serialized model tool-call argument bytes",
        &["frontend", "tool"],
        TOOL_SIZE_BUCKETS.to_vec()
    )
    .unwrap();

    pub(crate) static ref RESULT_BYTES: HistogramVec = register_histogram_vec!(
        TOOL_RESULT_BYTES,
        "Serialized tool result bytes returned into model history",
        &["frontend", "tool"],
        TOOL_RESULT_BUCKETS.to_vec()
    )
    .unwrap();

    static ref TOOL_LABELS: Mutex<HashSet<String>> = Mutex::new(
        ["unknown".to_string(), "other".to_string()]
            .into_iter()
            .collect()
    );
    static ref SEEN_TOOL_EVENTS: Mutex<SeenToolEvents> = Mutex::new(SeenToolEvents::default());
}

fn frontend_label(frontend: FrontendType) -> &'static str {
    match frontend {
        FrontendType::Codex => "codex",
        FrontendType::ClaudeCode => "claude_code",
    }
}

fn bounded_tool_label(label: String, labels: &mut HashSet<String>, limit: usize) -> String {
    if labels.contains(&label) {
        return label;
    }
    if labels.len() < limit {
        labels.insert(label.clone());
        return label;
    }
    "other".to_string()
}

fn sanitized_tool_name(name: Option<&str>) -> String {
    let name = name.unwrap_or("unknown");
    let bounded: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | ':') {
                character
            } else {
                '_'
            }
        })
        .take(120)
        .collect();
    let bounded = if bounded.is_empty() {
        "unknown".to_string()
    } else {
        bounded
    };
    let mut labels = TOOL_LABELS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    bounded_tool_label(bounded, &mut labels, MAX_TOOL_LABEL_CARDINALITY)
}

fn first_request_event(direction: &str, id: Option<&str>) -> bool {
    let Some(id) = id else {
        return true;
    };
    let bounded_id: String = id.chars().take(MAX_DEDUP_ID_CHARS).collect();
    let key = format!("{direction}:{bounded_id}");
    SEEN_TOOL_EVENTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .should_record(key)
}

fn serialized_len(value: &Value) -> u64 {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len() as u64)
        .unwrap_or(0)
}

fn record_call(frontend: FrontendType, tool: &str, direction: &'static str) {
    CALLS
        .with_label_values(&[frontend_label(frontend), tool, direction])
        .inc();
    persist_counter_inc(
        TOOL_CALLS_TOTAL,
        &[
            ("frontend", frontend_label(frontend)),
            ("tool", tool),
            ("direction", direction),
        ],
        1.0,
    );
}

fn observe_definition(frontend: FrontendType, tool: &str, bytes: u64) {
    DEFINITION_BYTES
        .with_label_values(&[frontend_label(frontend), tool])
        .observe(bytes as f64);
    persist_histogram_observe(
        TOOL_DEFINITION_BYTES,
        &[("frontend", frontend_label(frontend)), ("tool", tool)],
        bytes as f64,
    );
}

fn observe_arguments(frontend: FrontendType, tool: &str, bytes: u64) {
    ARGUMENT_BYTES
        .with_label_values(&[frontend_label(frontend), tool])
        .observe(bytes as f64);
    persist_histogram_observe(
        TOOL_ARGUMENT_BYTES,
        &[("frontend", frontend_label(frontend)), ("tool", tool)],
        bytes as f64,
    );
}

fn observe_result(frontend: FrontendType, tool: &str, bytes: u64) {
    RESULT_BYTES
        .with_label_values(&[frontend_label(frontend), tool])
        .observe(bytes as f64);
    persist_histogram_observe(
        TOOL_RESULT_BYTES,
        &[("frontend", frontend_label(frontend)), ("tool", tool)],
        bytes as f64,
    );
}

/// Record tool catalog and newly observed tool traffic without retaining payload content.
pub fn record_anthropic_request_tools(request: &Value, frontend: FrontendType) {
    for definition in request
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = sanitized_tool_name(definition.get("name").and_then(Value::as_str));
        observe_definition(frontend, &name, serialized_len(definition));
    }

    let tool_names_by_id: HashMap<&str, &str> = request
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|message| message.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
        .filter_map(|block| {
            let id = block.get("id")?.as_str()?;
            let name = block.get("name")?.as_str()?;
            Some((id, name))
        })
        .collect();

    for message in request
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let role = message.get("role").and_then(Value::as_str);
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            if role == Some("tool") {
                let id = message
                    .get("tool_use_id")
                    .or_else(|| message.get("tool_call_id"))
                    .and_then(Value::as_str);
                if !first_request_event("result", id) {
                    continue;
                }
                let name = sanitized_tool_name(id.and_then(|id| tool_names_by_id.get(id).copied()));
                record_call(frontend, &name, "result");
                observe_result(
                    frontend,
                    &name,
                    message
                        .get("content")
                        .map(serialized_len)
                        .unwrap_or_default(),
                );
            }
            continue;
        };

        for block in blocks {
            let block_type = block.get("type").and_then(Value::as_str);
            match block_type {
                Some("tool_use") => {
                    let id = block.get("id").and_then(Value::as_str);
                    if !first_request_event("call", id) {
                        continue;
                    }
                    let name = sanitized_tool_name(block.get("name").and_then(Value::as_str));
                    record_call(frontend, &name, "request");
                    let bytes = block.get("input").map(serialized_len).unwrap_or_default();
                    observe_arguments(frontend, &name, bytes);
                }
                Some("tool_result") => {
                    let id = block
                        .get("tool_use_id")
                        .or_else(|| block.get("tool_call_id"))
                        .and_then(Value::as_str);
                    if !first_request_event("result", id) {
                        continue;
                    }
                    let name = sanitized_tool_name(
                        block
                            .get("name")
                            .and_then(Value::as_str)
                            .or_else(|| id.and_then(|id| tool_names_by_id.get(id).copied())),
                    );
                    record_call(frontend, &name, "result");
                    let bytes = block.get("content").map(serialized_len).unwrap_or_default();
                    observe_result(frontend, &name, bytes);
                }
                _ => {}
            }
        }
    }
}

/// Observe Anthropic-shaped response tool calls without altering or retaining payloads.
pub async fn record_anthropic_response_tools_from_response(
    response: Response,
    frontend: FrontendType,
) -> Response {
    let (parts, body) = response.into_parts();
    let is_sse = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"));
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);

    tokio::spawn(async move {
        let mut stream = body.into_data_stream();
        let mut decoder = SseFrameDecoder::new();
        let mut calls: HashMap<u64, (String, u64)> = HashMap::new();
        let mut json_body = Vec::new();
        let mut observe_json = !is_sse;

        loop {
            tokio::select! {
                _ = tx.closed() => break,
                chunk = stream.next() => {
                    let Some(chunk) = chunk else { break; };
                    match chunk {
                        Ok(bytes) => {
                            if is_sse {
                                for frame in decoder.push(&bytes) {
                                    observe_stream_frame(&frame.data, frontend, &mut calls);
                                }
                            } else if observe_json {
                                if json_body.len().saturating_add(bytes.len()) > MAX_TELEMETRY_JSON_BYTES {
                                    observe_json = false;
                                    json_body.clear();
                                } else {
                                    json_body.extend_from_slice(&bytes);
                                }
                            }
                            if tx.send(Ok(bytes)).await.is_err() {
                                break;
                            }
                        }
                        Err(error) => {
                            let _ = tx.send(Err(std::io::Error::other(error.to_string()))).await;
                            break;
                        }
                    }
                }
            }
        }

        if observe_json {
            if let Ok(response) = serde_json::from_slice::<AnthropicResponse>(&json_body) {
                record_anthropic_response_tools(&response.content, frontend);
            }
        }
    });

    Response::from_parts(parts, Body::from_stream(ReceiverStream::new(rx)))
}

fn observe_stream_frame(
    data: &str,
    frontend: FrontendType,
    calls: &mut HashMap<u64, (String, u64)>,
) {
    let Ok(event) = serde_json::from_str::<Value>(data.trim()) else {
        return;
    };
    let index = event.get("index").and_then(Value::as_u64);
    match event.get("type").and_then(Value::as_str) {
        Some("content_block_start") => {
            let is_tool =
                event.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use");
            if let (Some(index), true, Some(name)) = (
                index,
                is_tool,
                event.pointer("/content_block/name").and_then(Value::as_str),
            ) {
                calls.insert(index, (name.to_string(), 0));
            }
        }
        Some("content_block_delta") => {
            if let (Some(index), Some(partial_json)) = (
                index,
                event.pointer("/delta/partial_json").and_then(Value::as_str),
            ) {
                if let Some((_, bytes)) = calls.get_mut(&index) {
                    *bytes = bytes.saturating_add(partial_json.len() as u64);
                }
            }
        }
        Some("content_block_stop") => {
            if let Some(index) = index {
                if let Some((name, bytes)) = calls.remove(&index) {
                    record_streamed_tool_call(frontend, &name, bytes);
                }
            }
        }
        Some("message_stop") => {
            for (_, (name, bytes)) in calls.drain() {
                record_streamed_tool_call(frontend, &name, bytes);
            }
        }
        _ => {}
    }
}

/// Record one assembled streaming tool call. Payload bytes are counted at the call site.
pub fn record_streamed_tool_call(frontend: FrontendType, tool: &str, argument_bytes: u64) {
    let tool = sanitized_tool_name(Some(tool));
    record_call(frontend, &tool, "response");
    observe_arguments(frontend, &tool, argument_bytes);
}

/// Record generated tool calls after a non-streaming Anthropic response is decoded.
pub fn record_anthropic_response_tools(content: &[AnthropicContentBlock], frontend: FrontendType) {
    for block in content {
        if let AnthropicContentBlock::ToolUse { name, input, .. } = block {
            let name = sanitized_tool_name(Some(name));
            record_call(frontend, &name, "response");
            observe_arguments(frontend, &name, serialized_len(input));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use prometheus::core::Collector;
    use serde_json::json;

    #[test]
    fn sanitizes_missing_and_unsafe_tool_names() {
        assert_eq!(sanitized_tool_name(None), "unknown");
        assert_eq!(sanitized_tool_name(Some("   ")), "___");
        assert_eq!(
            sanitized_tool_name(Some("exec command/a")),
            "exec_command_a"
        );
        let oversized = "x".repeat(160);
        assert_eq!(sanitized_tool_name(Some(&oversized)).len(), 120);
    }

    #[test]
    fn bounded_tool_label_caps_new_series_and_reuses_known_series() {
        let mut labels = HashSet::new();
        assert_eq!(
            bounded_tool_label("first".to_string(), &mut labels, 2),
            "first"
        );
        assert_eq!(
            bounded_tool_label("second".to_string(), &mut labels, 2),
            "second"
        );
        assert_eq!(
            bounded_tool_label("third".to_string(), &mut labels, 2),
            "other"
        );
        assert_eq!(
            bounded_tool_label("first".to_string(), &mut labels, 2),
            "first"
        );
    }

    #[tokio::test]
    async fn records_streamed_tool_call_for_json_handler() {
        record_streamed_tool_call(FrontendType::Codex, "direct_stream", 2);
        let response = crate::metrics::tool_metrics_handler().await.into_response();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("direct_stream"));
    }

    #[test]
    fn records_request_catalog_once_per_history_event_without_payload_storage() {
        let request = json!({
            "model": "mock,test",
            "messages": [{
                "role": "user",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "toolu_unit_catalog_1",
                        "name": "exec_unit_catalog",
                        "input": {"cmd": "private"}
                    },
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_unit_catalog_1",
                        "content": "private output"
                    }
                ]
            }],
            "tools": [{"name": "exec_unit_catalog", "description": "private"}]
        });
        let request_before = CALLS
            .with_label_values(&["codex", "exec_unit_catalog", "request"])
            .get();
        let result_before = CALLS
            .with_label_values(&["codex", "exec_unit_catalog", "result"])
            .get();

        record_anthropic_request_tools(&request, FrontendType::Codex);
        record_anthropic_request_tools(&request, FrontendType::Codex);

        assert_eq!(
            CALLS
                .with_label_values(&["codex", "exec_unit_catalog", "request"])
                .get()
                - request_before,
            1.0
        );
        assert_eq!(
            CALLS
                .with_label_values(&["codex", "exec_unit_catalog", "result"])
                .get()
                - result_before,
            1.0
        );
        assert!(
            DEFINITION_BYTES
                .with_label_values(&["codex", "exec_unit_catalog"])
                .get_sample_count()
                >= 2
        );
    }

    #[test]
    fn tool_role_array_content_is_counted_once() {
        let request = json!({
            "model": "mock,test",
            "messages": [
                {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "toolu_unit_array_1",
                        "name": "exec_unit_array",
                        "input": {"cmd": "private"}
                    }]
                },
                {
                    "role": "tool",
                    "tool_use_id": "toolu_unit_array_1",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "toolu_unit_array_1",
                        "content": "private output"
                    }]
                }
            ]
        });
        let before = CALLS
            .with_label_values(&["codex", "exec_unit_array", "result"])
            .get();
        record_anthropic_request_tools(&request, FrontendType::Codex);
        assert_eq!(
            CALLS
                .with_label_values(&["codex", "exec_unit_array", "result"])
                .get()
                - before,
            1.0
        );
    }

    #[tokio::test]
    async fn nonstream_response_tools_use_detected_frontend_without_payload_storage() {
        let body = json!({
            "id": "msg_unit_native_1",
            "type": "message",
            "role": "assistant",
            "model": "mock,test",
            "content": [{
                "type": "tool_use",
                "id": "toolu_unit_native_1",
                "name": "native_unit_exec",
                "input": {"cmd": "native-private-unit"}
            }],
            "usage": {"input_tokens": 3, "output_tokens": 2},
            "stop_reason": "tool_use"
        })
        .to_string();
        let response = Response::builder()
            .status(axum::http::StatusCode::OK)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap();

        let observed =
            record_anthropic_response_tools_from_response(response, FrontendType::ClaudeCode).await;
        let forwarded = axum::body::to_bytes(observed.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&forwarded).contains("native-private-unit"));

        let metrics_response = crate::metrics::tool_metrics_handler().await.into_response();
        let bytes = axum::body::to_bytes(metrics_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let raw = String::from_utf8_lossy(&bytes);
        let metrics: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        let entry = metrics
            .iter()
            .find(|entry| entry["frontend"] == "claude_code" && entry["tool"] == "native_unit_exec")
            .expect("native Claude response tool metric should exist");
        assert_eq!(
            entry["response_calls"]
                .as_u64()
                .expect("response_calls must be numeric"),
            1
        );
        assert!(!raw.contains("native-private-unit"));
    }

    #[test]
    fn unknown_tool_result_id_uses_bounded_unknown_label() {
        let request = json!({
            "model": "mock,test",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_unit_unmatched_unique_id",
                    "content": "private output"
                }]
            }]
        });
        let before = CALLS
            .with_label_values(&["codex", "unknown", "result"])
            .get();
        record_anthropic_request_tools(&request, FrontendType::Codex);
        assert_eq!(
            CALLS
                .with_label_values(&["codex", "unknown", "result"])
                .get()
                - before,
            1.0
        );
        assert!(!CALLS
            .collect()
            .iter()
            .flat_map(|family| family.get_metric())
            .any(|metric| metric.get_label().iter().any(|label| {
                label.name() == "tool" && label.value() == "toolu_unit_unmatched_unique_id"
            })));
    }
}
