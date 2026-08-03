// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use super::{
    add_reasoning_output_item, append_response_delta, initial_responses_output_item,
    map_openai_usage_to_responses_usage, response_output_item_identity, responses_reasoning_item,
    unique_response_output_item_identity, ResponseOutputItemIdentity,
};

#[derive(Default)]
struct ToolAccum {
    id: String,
    item_id: Option<String>,
    name: String,
    arguments: String,
    emitted_arguments_len: usize,
    added: bool,
    output_index: Option<usize>,
}

/// Stateful OpenAI/Anthropic SSE to Responses SSE converter.
///
/// Each complete upstream frame is parsed exactly once. Delta events are
/// returned immediately, while output-item and response terminal events are
/// emitted by [`Self::finish`].
pub(super) struct ResponsesStreamConverter {
    response_id: String,
    created_at: i64,
    model: String,
    created_sent: bool,
    reasoning_item_added: bool,
    reasoning_output_index: Option<usize>,
    message_item_added: bool,
    message_output_index: Option<usize>,
    next_output_index: usize,
    message_text: String,
    refusal_text: String,
    reasoning_text: String,
    response_status: String,
    incomplete_details: Option<serde_json::Value>,
    preserved_response: Option<serde_json::Value>,
    preserved_items_added: bool,
    tools: BTreeMap<usize, ToolAccum>,
    usage: serde_json::Value,
    finished: bool,
}

impl ResponsesStreamConverter {
    pub(super) fn new(preserved_response: Option<serde_json::Value>) -> Self {
        let response_status = preserved_response
            .as_ref()
            .and_then(|response| response.get("status"))
            .and_then(|status| status.as_str())
            .unwrap_or("completed")
            .to_string();
        Self {
            response_id: "resp_stream".to_string(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            model: "unknown".to_string(),
            created_sent: false,
            reasoning_item_added: false,
            reasoning_output_index: None,
            message_item_added: false,
            message_output_index: None,
            next_output_index: 0,
            message_text: String::new(),
            refusal_text: String::new(),
            reasoning_text: String::new(),
            response_status,
            incomplete_details: None,
            preserved_response,
            preserved_items_added: false,
            tools: BTreeMap::new(),
            usage: map_openai_usage_to_responses_usage(&serde_json::json!({})),
            finished: false,
        }
    }

    pub(super) fn response_id(&self) -> &str {
        &self.response_id
    }

    pub(super) fn push_frame(&mut self, event_type: Option<&str>, data: &str) -> String {
        if self.finished {
            return String::new();
        }
        let chunk: serde_json::Value = match serde_json::from_str(data) {
            Ok(value) => value,
            Err(_) => return String::new(),
        };
        let mut output = String::new();

        if !self.created_sent {
            if let Some(id) = chunk.get("id").and_then(|value| value.as_str()) {
                self.response_id = id.to_string();
            }
        }
        if let Some(created_at) = chunk.get("created").and_then(|value| value.as_i64()) {
            self.created_at = created_at;
        }
        if let Some(model) = chunk.get("model").and_then(|value| value.as_str()) {
            self.model = model.to_string();
        }
        if let Some(status) = chunk
            .get("response_status")
            .and_then(|value| value.as_str())
        {
            self.response_status = status.to_string();
        }
        if let Some(details) = chunk
            .get("incomplete_details")
            .filter(|value| !value.is_null())
        {
            self.incomplete_details = Some(details.clone());
        }

        if event_type == Some("message_start")
            || chunk.get("type").and_then(|value| value.as_str()) == Some("message_start")
        {
            if let Some(message) = chunk.get("message") {
                if !self.created_sent {
                    if let Some(id) = message.get("id").and_then(|value| value.as_str()) {
                        self.response_id = id.to_string();
                    }
                }
                if let Some(model) = message.get("model").and_then(|value| value.as_str()) {
                    self.model = model.to_string();
                }
                if let Some(usage) = message.get("usage") {
                    self.usage = anthropic_usage_to_responses_usage(usage);
                }
            }
        }

        self.ensure_created(&mut output);
        self.ensure_preserved_items(&mut output);

        if let Some(usage) = chunk.get("usage").filter(|usage| {
            !usage.is_null()
                && (usage.get("prompt_tokens").is_some()
                    || usage.get("completion_tokens").is_some())
        }) {
            self.usage = map_openai_usage_to_responses_usage(usage);
        }
        if let Some(choices) = chunk.get("choices").and_then(|value| value.as_array()) {
            if let Some(choice) = choices.first() {
                self.push_openai_choice(choice, &mut output);
            }
            return output;
        }

        let event_type = event_type.or_else(|| chunk.get("type").and_then(|value| value.as_str()));
        self.push_anthropic_event(event_type, &chunk, &mut output);
        output
    }

    fn ensure_created(&mut self, output: &mut String) {
        if self.created_sent {
            return;
        }
        let created = serde_json::json!({
            "type": "response.created",
            "response": {
                "id": self.response_id,
                "object": "response",
                "created_at": self.created_at,
                "status": "in_progress",
                "model": self.model
            }
        });
        output.push_str("event: response.created\ndata: ");
        output.push_str(&created.to_string());
        output.push_str("\n\n");
        self.created_sent = true;
    }

    fn ensure_preserved_items(&mut self, output: &mut String) {
        if self.preserved_items_added {
            return;
        }
        if let Some(items) = self
            .preserved_response
            .as_ref()
            .and_then(|response| response.get("output"))
            .and_then(|value| value.as_array())
        {
            for (output_index, item) in items.iter().enumerate() {
                let added = serde_json::json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                    "item": initial_responses_output_item(item)
                });
                output.push_str("event: response.output_item.added\ndata: ");
                output.push_str(&added.to_string());
                output.push_str("\n\n");
            }
            self.next_output_index = self.next_output_index.max(items.len());
            self.preserved_items_added = true;
        }
    }

    fn ensure_message_item(&mut self, output: &mut String) {
        if self.message_item_added {
            return;
        }
        if self.preserved_response.is_none() {
            let output_index = self.next_output_index;
            self.next_output_index += 1;
            let added = serde_json::json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": {
                    "id": format!("msg_{}", self.response_id),
                    "type": "message",
                    "role": "assistant",
                    "content": []
                }
            });
            output.push_str("event: response.output_item.added\ndata: ");
            output.push_str(&added.to_string());
            output.push_str("\n\n");
            self.message_output_index = Some(output_index);
        }
        self.message_item_added = true;
    }

    fn message_identity(&self, content_type: &str) -> Option<ResponseOutputItemIdentity> {
        self.preserved_response
            .as_ref()
            .and_then(|response| {
                unique_response_output_item_identity(response, "message", Some(content_type))
            })
            .or_else(|| {
                self.message_output_index
                    .map(|output_index| ResponseOutputItemIdentity {
                        output_index,
                        item_id: Some(format!("msg_{}", self.response_id)),
                        content_index: Some(0),
                    })
            })
    }

    fn reasoning_identity(&self) -> Option<ResponseOutputItemIdentity> {
        self.preserved_response
            .as_ref()
            .and_then(|response| {
                unique_response_output_item_identity(response, "reasoning", Some("reasoning_text"))
            })
            .or_else(|| {
                self.reasoning_output_index
                    .map(|output_index| ResponseOutputItemIdentity {
                        output_index,
                        item_id: Some(format!("rs_{}", self.response_id)),
                        content_index: Some(0),
                    })
            })
    }

    fn push_reasoning_delta(&mut self, reasoning: &str, output: &mut String) {
        if self.preserved_response.is_none() {
            add_reasoning_output_item(
                output,
                &self.response_id,
                &mut self.reasoning_item_added,
                &mut self.reasoning_output_index,
                &mut self.next_output_index,
            );
        } else {
            self.reasoning_item_added = true;
        }
        self.reasoning_text.push_str(reasoning);
        append_response_delta(
            output,
            "response.reasoning_text.delta",
            reasoning,
            self.reasoning_identity(),
        );
    }

    fn push_openai_choice(&mut self, choice: &serde_json::Value, output: &mut String) {
        let delta = choice.get("delta").cloned().unwrap_or_default();

        if let Some(text) = delta
            .get("content")
            .and_then(|value| value.as_str())
            .filter(|text| !text.is_empty())
        {
            self.ensure_message_item(output);
            self.message_text.push_str(text);
            append_response_delta(
                output,
                "response.output_text.delta",
                text,
                self.message_identity("output_text"),
            );
        }

        if let Some(reasoning) = delta
            .get("reasoning_content")
            .and_then(|value| value.as_str())
            .filter(|reasoning| !reasoning.is_empty())
        {
            self.push_reasoning_delta(reasoning, output);
        }

        if let Some(refusal) = delta
            .get("refusal")
            .and_then(|value| value.as_str())
            .filter(|refusal| !refusal.is_empty())
        {
            self.ensure_message_item(output);
            self.refusal_text.push_str(refusal);
            append_response_delta(
                output,
                "response.refusal.delta",
                refusal,
                self.message_identity("refusal"),
            );
        }

        let Some(tool_calls) = delta.get("tool_calls").and_then(|value| value.as_array()) else {
            return;
        };
        for tool_call in tool_calls {
            let index = tool_call
                .get("index")
                .and_then(|value| value.as_u64())
                .unwrap_or(0) as usize;
            let should_add = {
                let entry = self.tools.entry(index).or_default();
                if let Some(id) = tool_call.get("id").and_then(|value| value.as_str()) {
                    entry.id = id.to_string();
                }
                if let Some(name) = tool_call
                    .get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(|value| value.as_str())
                {
                    entry.name = name.to_string();
                }
                if let Some(arguments) = tool_call
                    .get("function")
                    .and_then(|function| function.get("arguments"))
                    .and_then(|value| value.as_str())
                {
                    entry.arguments.push_str(arguments);
                }
                !entry.added && (!entry.id.is_empty() || !entry.name.is_empty())
            };
            if should_add {
                self.ensure_tool_item(index, output);
            }
            self.emit_pending_tool_arguments(index, output);
        }
    }

    fn ensure_tool_item(&mut self, index: usize, output: &mut String) {
        let entry = self.tools.entry(index).or_default();
        if entry.added {
            return;
        }
        if entry.id.is_empty() {
            entry.id = format!("call_{index}");
        }
        if entry.name.is_empty() {
            entry.name = "tool".to_string();
        }
        if self.preserved_response.is_none() {
            let output_index = self.next_output_index;
            self.next_output_index += 1;
            let added = serde_json::json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": {
                    "id": entry.id,
                    "type": "function_call",
                    "call_id": entry.id,
                    "name": entry.name,
                    "arguments": ""
                }
            });
            output.push_str("event: response.output_item.added\ndata: ");
            output.push_str(&added.to_string());
            output.push_str("\n\n");
            entry.output_index = Some(output_index);
            entry.item_id = Some(entry.id.clone());
        } else if let Some(identity) = self.preserved_response.as_ref().and_then(|response| {
            response_output_item_identity(response, "function_call", index, None)
        }) {
            entry.output_index = Some(identity.output_index);
            entry.item_id = identity.item_id;
        }
        entry.added = true;
    }

    fn emit_pending_tool_arguments(&mut self, index: usize, output: &mut String) {
        let Some(entry) = self.tools.get_mut(&index) else {
            return;
        };
        if !entry.added || entry.arguments.len() <= entry.emitted_arguments_len {
            return;
        }
        let arguments = entry.arguments[entry.emitted_arguments_len..].to_string();
        entry.emitted_arguments_len = entry.arguments.len();
        let identity = entry
            .output_index
            .map(|output_index| ResponseOutputItemIdentity {
                output_index,
                item_id: entry.item_id.clone(),
                content_index: None,
            });
        append_response_delta(
            output,
            "response.function_call_arguments.delta",
            &arguments,
            identity,
        );
    }

    fn push_anthropic_tool_start(&mut self, chunk: &serde_json::Value, output: &mut String) {
        let Some(block) = chunk.get("content_block") else {
            return;
        };
        if block.get("type").and_then(|value| value.as_str()) != Some("tool_use") {
            return;
        }
        let index = chunk
            .get("index")
            .and_then(|value| value.as_u64())
            .unwrap_or(0) as usize;
        let entry = self.tools.entry(index).or_default();
        if let Some(id) = block.get("id").and_then(|value| value.as_str()) {
            entry.id = id.to_string();
        }
        if let Some(name) = block.get("name").and_then(|value| value.as_str()) {
            entry.name = name.to_string();
        }
        if let Some(input) = block.get("input").filter(|input| {
            !input.is_null() && !input.as_object().is_some_and(serde_json::Map::is_empty)
        }) {
            entry.arguments.push_str(&input.to_string());
        }
        self.ensure_tool_item(index, output);
        self.emit_pending_tool_arguments(index, output);
    }

    fn push_anthropic_tool_delta(
        &mut self,
        chunk: &serde_json::Value,
        delta: &serde_json::Value,
        output: &mut String,
    ) {
        let Some(arguments) = delta
            .get("partial_json")
            .and_then(|value| value.as_str())
            .filter(|arguments| !arguments.is_empty())
        else {
            return;
        };
        let index = chunk
            .get("index")
            .and_then(|value| value.as_u64())
            .unwrap_or(0) as usize;
        {
            let entry = self.tools.entry(index).or_default();
            entry.arguments.push_str(arguments);
        }
        self.ensure_tool_item(index, output);
        self.emit_pending_tool_arguments(index, output);
    }

    fn push_anthropic_event(
        &mut self,
        event_type: Option<&str>,
        chunk: &serde_json::Value,
        output: &mut String,
    ) {
        match event_type {
            Some("content_block_start") => self.push_anthropic_tool_start(chunk, output),
            Some("content_block_delta") => {
                let Some(delta) = chunk.get("delta") else {
                    return;
                };
                if delta.get("type").and_then(|value| value.as_str()) == Some("input_json_delta") {
                    self.push_anthropic_tool_delta(chunk, delta, output);
                    return;
                }
                if let Some(text) = delta
                    .get("text")
                    .and_then(|value| value.as_str())
                    .filter(|text| !text.is_empty())
                {
                    self.ensure_message_item(output);
                    self.message_text.push_str(text);
                    append_response_delta(
                        output,
                        "response.output_text.delta",
                        text,
                        self.message_identity("output_text"),
                    );
                }
                if let Some(thinking) = delta
                    .get("thinking")
                    .and_then(|value| value.as_str())
                    .filter(|thinking| !thinking.is_empty())
                {
                    self.push_reasoning_delta(thinking, output);
                }
            }
            Some("message_delta") => {
                if let Some(usage) = chunk.get("usage") {
                    self.usage = merge_anthropic_usage(&self.usage, usage);
                }
            }
            _ => {}
        }
    }

    pub(super) fn finish(&mut self) -> String {
        if self.finished {
            return String::new();
        }
        self.finished = true;
        let mut output = String::new();
        let mut indexed_output_items = Vec::new();

        if self.preserved_response.is_none() && !self.reasoning_text.is_empty() {
            let reasoning_item = responses_reasoning_item(&self.response_id, &self.reasoning_text);
            let output_index = self
                .reasoning_output_index
                .expect("generated reasoning output must have an added event");
            indexed_output_items.push((output_index, reasoning_item.clone()));
            append_output_item_done(&mut output, output_index, &reasoning_item);
        }

        if self.preserved_response.is_none() && self.message_item_added {
            let mut content = Vec::new();
            if !self.message_text.is_empty() {
                content.push(serde_json::json!({
                    "type": "output_text",
                    "text": self.message_text
                }));
            }
            if !self.refusal_text.is_empty() {
                content.push(serde_json::json!({
                    "type": "refusal",
                    "refusal": self.refusal_text
                }));
            }
            let message_item = serde_json::json!({
                "id": format!("msg_{}", self.response_id),
                "type": "message",
                "role": "assistant",
                "content": content
            });
            let output_index = self
                .message_output_index
                .expect("generated message output must have an added event");
            indexed_output_items.push((output_index, message_item.clone()));
            append_output_item_done(&mut output, output_index, &message_item);
        }

        let mut next_output_index = self.next_output_index;
        for tool in self
            .tools
            .values()
            .filter(|_| self.preserved_response.is_none())
        {
            let call_id = if tool.id.is_empty() {
                "call_unknown"
            } else {
                &tool.id
            };
            let name = if tool.name.is_empty() {
                "tool"
            } else {
                &tool.name
            };
            let item = serde_json::json!({
                "id": call_id,
                "type": "function_call",
                "call_id": call_id,
                "name": name,
                "arguments": tool.arguments
            });
            let output_index = tool.output_index.unwrap_or_else(|| {
                let output_index = next_output_index;
                next_output_index += 1;
                let added = serde_json::json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                    "item": {
                        "id": call_id,
                        "type": "function_call",
                        "call_id": call_id,
                        "name": name,
                        "arguments": ""
                    }
                });
                output.push_str("event: response.output_item.added\ndata: ");
                output.push_str(&added.to_string());
                output.push_str("\n\n");
                output_index
            });
            indexed_output_items.push((output_index, item.clone()));
            append_output_item_done(&mut output, output_index, &item);
        }

        indexed_output_items.sort_by_key(|(output_index, _)| *output_index);
        let output_items = indexed_output_items
            .into_iter()
            .map(|(_, item)| item)
            .collect::<Vec<_>>();

        if let Some(items) = self
            .preserved_response
            .as_ref()
            .and_then(|response| response.get("output"))
            .and_then(|value| value.as_array())
        {
            for (output_index, item) in items.iter().enumerate() {
                append_output_item_done(&mut output, output_index, item);
            }
        }

        let terminal_type = match self.response_status.as_str() {
            "queued" => "response.queued",
            "in_progress" => "response.in_progress",
            "completed" => "response.completed",
            "incomplete" => "response.incomplete",
            "failed" => "response.failed",
            "cancelled" => "response.cancelled",
            _ => "response.incomplete",
        };
        let terminal_response = self.preserved_response.clone().unwrap_or_else(|| {
            let mut response = serde_json::json!({
                "id": self.response_id,
                "object": "response",
                "created_at": self.created_at,
                "status": self.response_status,
                "model": self.model,
                "output": output_items,
                "usage": self.usage
            });
            if let Some(details) = &self.incomplete_details {
                response["incomplete_details"] = details.clone();
            }
            response
        });
        let terminal = serde_json::json!({
            "type": terminal_type,
            "response": terminal_response
        });
        output.push_str("event: ");
        output.push_str(terminal_type);
        output.push_str("\ndata: ");
        output.push_str(&terminal.to_string());
        output.push_str("\n\n");
        output
    }
}

fn anthropic_usage_to_responses_usage(usage: &serde_json::Value) -> serde_json::Value {
    let input_tokens = usage
        .get("input_tokens")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let cached_tokens = usage
        .get("cache_read_input_tokens")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let reasoning_tokens = usage
        .get("reasoning_tokens")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    map_openai_usage_to_responses_usage(&serde_json::json!({
        "prompt_tokens": input_tokens,
        "completion_tokens": output_tokens,
        "total_tokens": input_tokens + output_tokens,
        "prompt_tokens_details": {"cached_tokens": cached_tokens},
        "completion_tokens_details": {"reasoning_tokens": reasoning_tokens}
    }))
}

fn merge_anthropic_usage(
    current: &serde_json::Value,
    update: &serde_json::Value,
) -> serde_json::Value {
    let input_tokens = update
        .get("input_tokens")
        .and_then(|value| value.as_u64())
        .or_else(|| current.get("input_tokens").and_then(|value| value.as_u64()))
        .unwrap_or(0);
    let output_tokens = update
        .get("output_tokens")
        .and_then(|value| value.as_u64())
        .or_else(|| {
            current
                .get("output_tokens")
                .and_then(|value| value.as_u64())
        })
        .unwrap_or(0);
    let cached_tokens = update
        .get("cache_read_input_tokens")
        .and_then(|value| value.as_u64())
        .or_else(|| {
            current
                .get("input_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .and_then(|value| value.as_u64())
        })
        .unwrap_or(0);
    let reasoning_tokens = update
        .get("reasoning_tokens")
        .and_then(|value| value.as_u64())
        .or_else(|| {
            current
                .get("output_tokens_details")
                .and_then(|details| details.get("reasoning_tokens"))
                .and_then(|value| value.as_u64())
        })
        .unwrap_or(0);
    anthropic_usage_to_responses_usage(&serde_json::json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "cache_read_input_tokens": cached_tokens,
        "reasoning_tokens": reasoning_tokens
    }))
}

fn append_output_item_done(output: &mut String, output_index: usize, item: &serde_json::Value) {
    let done = serde_json::json!({
        "type": "response.output_item.done",
        "output_index": output_index,
        "item": item
    });
    output.push_str("event: response.output_item.done\ndata: ");
    output.push_str(&done.to_string());
    output.push_str("\n\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserved_items_reserve_their_output_indices() {
        let preserved = serde_json::json!({
            "id": "resp_preserved",
            "output": [
                {"id": "item_0", "type": "reasoning", "summary": []},
                {"id": "item_1", "type": "message", "role": "assistant", "content": []}
            ]
        });
        let mut converter = ResponsesStreamConverter::new(Some(preserved));
        let frame = serde_json::json!({
            "id": "resp_preserved",
            "choices": []
        });

        converter.push_frame(None, &frame.to_string());

        assert_eq!(converter.next_output_index, 2);
    }
}
