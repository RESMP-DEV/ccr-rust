// SPDX-License-Identifier: AGPL-3.0-or-later

use super::ResponseOutputItemIdentity;

fn append_event(output: &mut String, event_type: &str, event: serde_json::Value) {
    output.push_str("event: ");
    output.push_str(event_type);
    output.push_str("\ndata: ");
    output.push_str(&event.to_string());
    output.push_str("\n\n");
}

fn event_with_identity(
    event_type: &str,
    identity: &ResponseOutputItemIdentity,
) -> Option<serde_json::Value> {
    let content_index = identity.content_index?;
    let mut event = serde_json::json!({
        "type": event_type,
        "output_index": identity.output_index,
        "content_index": content_index
    });
    if let Some(item_id) = &identity.item_id {
        event["item_id"] = serde_json::Value::String(item_id.clone());
    }
    Some(event)
}

fn content_part(content_type: &str, content: &str) -> serde_json::Value {
    match content_type {
        "refusal" => serde_json::json!({"type": "refusal", "refusal": content}),
        _ => serde_json::json!({
            "type": "output_text",
            "text": content,
            "annotations": []
        }),
    }
}

pub(super) fn append_content_part_added(
    output: &mut String,
    identity: &ResponseOutputItemIdentity,
    content_type: &str,
) {
    let Some(mut event) = event_with_identity("response.content_part.added", identity) else {
        return;
    };
    event["part"] = content_part(content_type, "");
    append_event(output, "response.content_part.added", event);
}

pub(super) fn append_content_part_done(
    output: &mut String,
    identity: &ResponseOutputItemIdentity,
    content_type: &str,
    content: &str,
    preserved_part: Option<&serde_json::Value>,
) {
    let (delta_done_type, value_key) = match content_type {
        "refusal" => ("response.refusal.done", "refusal"),
        _ => ("response.output_text.done", "text"),
    };
    let completed_content = preserved_part
        .and_then(|part| part.get(value_key))
        .and_then(|value| value.as_str())
        .unwrap_or(content);
    if let Some(mut event) = event_with_identity(delta_done_type, identity) {
        event[value_key] = serde_json::Value::String(completed_content.to_string());
        append_event(output, delta_done_type, event);
    }

    if let Some(mut event) = event_with_identity("response.content_part.done", identity) {
        event["part"] = preserved_part
            .cloned()
            .unwrap_or_else(|| content_part(content_type, completed_content));
        append_event(output, "response.content_part.done", event);
    }
}
