use prometheus::{register_counter_vec, register_histogram_vec, CounterVec, HistogramVec};
use serde_json::Value;

use crate::frontend::FrontendType;
use crate::router::AnthropicContentBlock;

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
}

fn frontend_label(frontend: FrontendType) -> &'static str {
    match frontend {
        FrontendType::Codex => "codex",
        FrontendType::ClaudeCode => "claude_code",
    }
}

fn sanitized_tool_name(name: Option<&str>) -> String {
    let name = name.unwrap_or("unknown");
    let sanitized: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | ':') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let mut bounded = String::new();
    for character in sanitized.chars().take(128) {
        if bounded.len() >= 120 {
            break;
        }
        bounded.push(character);
    }
    if bounded.is_empty() {
        bounded.push_str("unknown");
    }
    bounded
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

/// Record tool catalog and historical tool traffic without retaining payload content.
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

    let tool_names_by_id: std::collections::HashMap<&str, &str> = request
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
                let id = message.get("tool_call_id").and_then(Value::as_str);
                let name = id.and_then(|id| tool_names_by_id.get(id).copied()).or(id);
                record_call(frontend, &sanitized_tool_name(name), "result");
                observe_result(
                    frontend,
                    &sanitized_tool_name(name),
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
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .or_else(|| id.and_then(|id| tool_names_by_id.get(id).copied()))
                        .or(id);
                    record_call(frontend, &sanitized_tool_name(name), "result");
                    let bytes = block.get("content").map(serialized_len).unwrap_or_default();
                    observe_result(frontend, &sanitized_tool_name(name), bytes);
                }
                _ => {}
            }
        }

        if role == Some("tool") {
            let id = message.get("tool_call_id").and_then(Value::as_str);
            let name = id.and_then(|id| tool_names_by_id.get(id).copied()).or(id);
            record_call(frontend, &sanitized_tool_name(name), "result");
            observe_result(
                frontend,
                &sanitized_tool_name(name),
                message
                    .get("content")
                    .map(serialized_len)
                    .unwrap_or_default(),
            );
        }
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
    use serde_json::json;

    #[test]
    fn sanitizes_missing_and_unsafe_tool_names() {
        assert_eq!(sanitized_tool_name(None), "unknown");
        assert_eq!(sanitized_tool_name(Some("   ")), "___");
        assert_eq!(
            sanitized_tool_name(Some("exec command/a")),
            "exec_command_a"
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
    fn records_request_catalog_and_history_without_payload_storage() {
        let request = json!({
            "model": "mock,test",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "tool_use", "name": "exec", "input": {"cmd": "private"}},
                    {"type": "tool_result", "name": "exec", "content": "private output"}
                ]
            }],
            "tools": [{"name": "exec", "description": "private"}]
        });
        record_anthropic_request_tools(&request, FrontendType::Codex);

        assert!(CALLS.with_label_values(&["codex", "exec", "request"]).get() >= 1.0);
        assert!(CALLS.with_label_values(&["codex", "exec", "result"]).get() >= 1.0);
        assert!(
            DEFINITION_BYTES
                .with_label_values(&["codex", "exec"])
                .get_sample_count()
                >= 1
        );
    }
}
