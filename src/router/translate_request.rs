// SPDX-License-Identifier: AGPL-3.0-or-later
use super::types::*;
use tracing::debug;

// ============================================================================
// Request Translation: Anthropic -> OpenAI
// ============================================================================

/// Convert Anthropic message content to OpenAI content format.
///
/// Preserves multimodal blocks where possible:
/// - text blocks => OpenAI text blocks
/// - image blocks => OpenAI image_url blocks
/// - thinking blocks => rendered inline as text for compatibility
pub(super) fn normalize_message_content(content: &serde_json::Value) -> serde_json::Value {
    match content {
        serde_json::Value::String(s) => serde_json::Value::String(s.clone()),
        serde_json::Value::Array(arr) => {
            let mut openai_blocks = Vec::new();
            let mut text_fallback = String::new();
            for block in arr {
                let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match block_type {
                    "text" => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            openai_blocks.push(serde_json::json!({
                                "type": "text",
                                "text": text
                            }));
                        }
                    }
                    "image" => {
                        if let Some(source) = block.get("source") {
                            if let Some(data) = source.get("data").and_then(|v| v.as_str()) {
                                let media_type = source
                                    .get("media_type")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("image/jpeg");
                                openai_blocks.push(serde_json::json!({
                                    "type": "image_url",
                                    "image_url": {
                                        "url": format!("data:{};base64,{}", media_type, data)
                                    }
                                }));
                            } else if let Some(url) = source.get("url").and_then(|v| v.as_str()) {
                                openai_blocks.push(serde_json::json!({
                                    "type": "image_url",
                                    "image_url": {
                                        "url": url
                                    }
                                }));
                            }
                        }
                    }
                    "image_url" => {
                        // OpenAI-style image block already in OpenAI form
                        // (arrives here from Codex/chat frontends whose raw
                        // content was kept). Preserve it instead of letting
                        // the unknown-block fallback stringify the payload
                        // as text.
                        openai_blocks.push(block.clone());
                    }
                    "thinking" => {
                        if let Some(thinking) = block.get("thinking").and_then(|t| t.as_str()) {
                            if !text_fallback.is_empty() {
                                text_fallback.push_str("\n\n");
                            }
                            text_fallback.push_str("<thinking>");
                            text_fallback.push_str(thinking);
                            text_fallback.push_str("</thinking>");
                        }
                    }
                    _ => {
                        if !text_fallback.is_empty() {
                            text_fallback.push('\n');
                        }
                        text_fallback.push_str(&block.to_string());
                    }
                }
            }

            let text_only_blocks = !openai_blocks.is_empty()
                && text_fallback.is_empty()
                && openai_blocks
                    .iter()
                    .all(|block| block.get("type").and_then(|t| t.as_str()) == Some("text"));

            if text_only_blocks {
                let concatenated_text = openai_blocks
                    .iter()
                    .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                    .collect::<String>();
                serde_json::Value::String(concatenated_text)
            } else if openai_blocks.is_empty() {
                serde_json::Value::String(text_fallback)
            } else {
                if !text_fallback.is_empty() {
                    openai_blocks.push(serde_json::json!({
                        "type": "text",
                        "text": text_fallback
                    }));
                }
                serde_json::Value::Array(openai_blocks)
            }
        }
        _ => content.clone(),
    }
}

pub(super) fn has_nonempty_content(content: &serde_json::Value) -> bool {
    match content {
        serde_json::Value::Null => false,
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Object(o) => !o.is_empty(),
        _ => true,
    }
}

/// Convert an OpenAI-style `image_url` content block into the Anthropic
/// `image` block form. `"image_url"` may be a plain string or an object
/// with a `url` field (the OpenAI chat-completions shape). Returns `None`
/// for anything that is not an OpenAI image block.
///
/// `data:<media_type>;base64,<payload>` sources become Anthropic base64
/// sources; every other URL becomes an Anthropic url source.
pub(super) fn openai_image_block_to_anthropic(
    block: &serde_json::Value,
) -> Option<serde_json::Value> {
    if block.get("type").and_then(|t| t.as_str()) != Some("image_url") {
        return None;
    }
    let image_url = block.get("image_url")?;
    let url = image_url
        .as_str()
        .or_else(|| image_url.get("url").and_then(|u| u.as_str()))?;
    if let Some(rest) = url.strip_prefix("data:") {
        if let Some((raw_media_type, data)) = rest.split_once(";base64,") {
            if !data.is_empty() {
                // Anthropic accepts only bare image media types; strip data
                // URL parameters (e.g. `image/png;charset=utf-8`) and
                // normalize the common `image/jpg` alias.
                let bare_media_type = raw_media_type
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim();
                let media_type = match bare_media_type {
                    "" => "image/jpeg",
                    "image/jpg" => "image/jpeg",
                    other => other,
                };
                return Some(serde_json::json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": media_type,
                        "data": data
                    }
                }));
            }
        }
    }
    Some(serde_json::json!({
        "type": "image",
        "source": {
            "type": "url",
            "url": url
        }
    }))
}

/// Convert Anthropic tool format to OpenAI tool format.
///
/// Anthropic: `{name, description, input_schema}`
/// OpenAI: `{type: "function", function: {name, description, parameters}}`
pub(super) fn convert_anthropic_tools_to_openai(
    tools: &Option<Vec<serde_json::Value>>,
) -> Option<Vec<serde_json::Value>> {
    tools.as_ref().map(|tool_list| {
        tool_list
            .iter()
            .map(|tool| {
                // Check if already in OpenAI format (has "type": "function")
                if tool.get("type").and_then(|t| t.as_str()) == Some("function") {
                    return tool.clone();
                }

                // Convert from Anthropic format to OpenAI format
                let name = tool.get("name").cloned().unwrap_or(serde_json::Value::Null);
                let description = tool.get("description").cloned();
                let parameters = tool
                    .get("input_schema")
                    .cloned()
                    .unwrap_or(serde_json::json!({"type": "object", "properties": {}}));

                let mut function = serde_json::json!({
                    "name": name,
                    "parameters": parameters,
                });

                if let Some(desc) = description {
                    function["description"] = desc;
                }

                serde_json::json!({
                    "type": "function",
                    "function": function,
                })
            })
            .collect()
    })
}

/// Translate Anthropic request format to OpenAI format.
pub(super) fn translate_request_anthropic_to_openai(
    anthropic_req: &AnthropicRequest,
    model: &str,
) -> OpenAIRequest {
    debug!(
        model,
        message_count = anthropic_req.messages.len(),
        "translating Anthropic request to OpenAI format"
    );
    let mut messages: Vec<OpenAIMessage> = Vec::new();
    let model_lower = model.to_lowercase();
    let is_reasoning_model = model_lower.contains("reasoner")
        || model_lower.contains("r1")
        || model_lower.contains("thinking");
    let is_deepseek = model_lower.contains("deepseek");

    // Handle system prompt: Anthropic has it as a top-level field,
    // OpenAI expects it as the first message with role "system"
    if let Some(system) = &anthropic_req.system {
        let system_content = match system {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(arr) => {
                let mut result = String::new();
                for block in arr {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        if !result.is_empty() {
                            result.push('\n');
                        }
                        result.push_str(text);
                    }
                }
                result
            }
            _ => system.to_string(),
        };

        if !system_content.is_empty() {
            messages.push(OpenAIMessage {
                role: "system".to_string(),
                content: Some(serde_json::Value::String(system_content)),
                reasoning_content: None,
                tool_call_id: None,
                tool_calls: None,
            });
        }
    }

    // Convert user and assistant messages
    for msg in &anthropic_req.messages {
        let role = match msg.role.as_str() {
            "human" | "user" => "user",
            "assistant" => "assistant",
            r => r,
        };

        // Handle tool_result blocks in user messages
        if let Some(arr) = msg.content.as_array() {
            for block in arr {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                    // Extract content and tool_use_id from tool_result block
                    let tool_content = block.get("content").and_then(|c| {
                        // Content can be a string or an object with type "text"
                        if let Some(s) = c.as_str() {
                            Some(s.to_string())
                        } else if let Some(obj) = c.as_object() {
                            obj.get("text").and_then(|t| t.as_str()).map(String::from)
                        } else if let Some(arr) = c.as_array() {
                            // Handle array of content items in tool_result
                            let mut result = String::new();
                            for item in arr {
                                if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                    result.push_str(text);
                                } else if let Some(text) = item.as_str() {
                                    result.push_str(text);
                                }
                            }
                            Some(result)
                        } else {
                            None
                        }
                    });

                    let tool_call_id = block
                        .get("tool_use_id")
                        .and_then(|id| id.as_str())
                        .map(String::from);

                    messages.push(OpenAIMessage {
                        role: "tool".to_string(),
                        content: tool_content.map(serde_json::Value::String),
                        tool_call_id,
                        ..Default::default()
                    });
                }
            }
        }

        let mut content_source = msg.content.clone();
        let mut tool_calls: Option<Vec<OpenAIToolCall>> = None;
        let mut reasoning_content: Option<String> = None;

        // Convert Anthropic assistant tool_use blocks into OpenAI tool_calls.
        // Keep non-tool content blocks as assistant content.
        if role == "assistant" {
            if let Some(arr) = msg.content.as_array() {
                let mut filtered_blocks: Vec<serde_json::Value> = Vec::new();
                let mut converted_tool_calls: Vec<OpenAIToolCall> = Vec::new();
                let mut reasoning_parts: Vec<String> = Vec::new();

                for block in arr {
                    match block.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                        "tool_use" => {
                            let id = block
                                .get("id")
                                .and_then(|v| v.as_str())
                                .filter(|v| !v.is_empty())
                                .unwrap_or("toolu_unknown")
                                .to_string();
                            let name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .filter(|v| !v.is_empty())
                                .unwrap_or("tool")
                                .to_string();
                            let arguments = block
                                .get("input")
                                .cloned()
                                .unwrap_or_else(|| serde_json::json!({}))
                                .to_string();

                            converted_tool_calls.push(OpenAIToolCall {
                                id: Some(id),
                                tool_type: Some("function".to_string()),
                                function: Some(OpenAIToolFunction { name, arguments }),
                            });
                        }
                        "thinking" => {
                            if let Some(thinking) = block.get("thinking").and_then(|t| t.as_str()) {
                                if !thinking.is_empty() {
                                    reasoning_parts.push(thinking.to_string());
                                }
                            }
                        }
                        _ => filtered_blocks.push(block.clone()),
                    }
                }

                if !converted_tool_calls.is_empty() {
                    tool_calls = Some(converted_tool_calls);
                    content_source = serde_json::Value::Array(filtered_blocks);
                }
                if !reasoning_parts.is_empty() {
                    reasoning_content = Some(reasoning_parts.join("\n\n"));
                }
            }

            // DeepSeek reasoning models require `reasoning_content` to be present
            // on assistant turns that participate in tool calling.
            if is_reasoning_model && reasoning_content.is_none() {
                reasoning_content = Some(String::new());
            }
        }

        // Handle regular message content
        let content = normalize_message_content(&content_source);

        if has_nonempty_content(&content) || role != "user" {
            messages.push(OpenAIMessage {
                role: role.to_string(),
                content: Some(content),
                reasoning_content,
                tool_call_id: msg.tool_call_id.clone(),
                tool_calls,
            });
        }
    }

    OpenAIRequest {
        model: model.to_string(),
        messages,
        // Use max_completion_tokens for reasoning models to allow for reasoning
        max_tokens: if is_reasoning_model {
            None
        } else {
            anthropic_req.max_tokens
        },
        max_completion_tokens: if is_reasoning_model {
            anthropic_req.max_tokens
        } else {
            None
        },
        temperature: anthropic_req.temperature,
        stream: anthropic_req.stream,
        tools: convert_anthropic_tools_to_openai(&anthropic_req.tools),
        reasoning_effort: if is_reasoning_model {
            Some("high".to_string())
        } else {
            None
        },
        thinking: if is_deepseek && !is_reasoning_model {
            Some(serde_json::json!({"type": "disabled"}))
        } else {
            None
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_image_block_string_data_url_becomes_base64_source() {
        let block = json!({
            "type": "image_url",
            "image_url": "data:image/png;base64,AAAA"
        });
        let converted = openai_image_block_to_anthropic(&block).unwrap();
        assert_eq!(converted["type"], "image");
        assert_eq!(converted["source"]["type"], "base64");
        assert_eq!(converted["source"]["media_type"], "image/png");
        assert_eq!(converted["source"]["data"], "AAAA");
    }

    #[test]
    fn openai_image_block_object_url_becomes_url_source() {
        let block = json!({
            "type": "image_url",
            "image_url": {"url": "https://example.test/pic.png", "detail": "low"}
        });
        let converted = openai_image_block_to_anthropic(&block).unwrap();
        assert_eq!(converted["type"], "image");
        assert_eq!(converted["source"]["type"], "url");
        assert_eq!(converted["source"]["url"], "https://example.test/pic.png");
    }

    #[test]
    fn openai_image_block_normalizes_anthropic_media_types() {
        let with_params = json!({
            "type": "image_url",
            "image_url": "data:image/png;charset=utf-8;base64,AAAA"
        });
        let converted = openai_image_block_to_anthropic(&with_params).unwrap();
        assert_eq!(converted["source"]["media_type"], "image/png");

        let jpg_alias = json!({
            "type": "image_url",
            "image_url": {"url": "data:image/jpg;base64,AAAA"}
        });
        let converted = openai_image_block_to_anthropic(&jpg_alias).unwrap();
        assert_eq!(converted["source"]["media_type"], "image/jpeg");
    }

    #[test]
    fn openai_image_block_ignores_non_image_blocks() {
        assert!(openai_image_block_to_anthropic(&json!({"type": "text", "text": "hi"})).is_none());
        assert!(openai_image_block_to_anthropic(&json!({"type": "image_url"})).is_none());
    }

    #[test]
    fn normalize_message_content_preserves_openai_image_blocks() {
        let content = json!([
            {"type": "text", "text": "look:"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}
        ]);
        let converted = normalize_message_content(&content);
        let blocks = converted.as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1]["type"], "image_url");
        assert_eq!(blocks[1]["image_url"]["url"], "data:image/png;base64,AAAA");
    }
}
