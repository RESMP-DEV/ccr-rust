// SPDX-License-Identifier: AGPL-3.0-or-later
//! GLM (Z.AI) transformer for modern Z.AI API.
//!
//! Handles Z.AI-specific request/response transformations:
//! - Request: inject `reasoning_effort` parameter for GLM-5.2+ models
//! - Request: strip Anthropic-specific passthrough fields
//! - Response: extract legacy `` tags for backward compatibility
//! - Response: preserve modern `reasoning_content` field
//!
//! Modern Z.AI models (GLM-5.2+) return `reasoning_content` directly in
//! OpenAI-compatible format. Older models may use `` tags which
//! are extracted and normalized to `reasoning_content`.
//!
//! GLM-5.2 supports `reasoning_effort` values: "low", "medium", "high"

use crate::transformer::Transformer;
use anyhow::Result;
use regex::Regex;
use serde_json::Value;
use std::sync::{Arc, LazyLock, Mutex};
use tracing::trace;

const THINK_START_TAG: &str = "<think>";
const THINK_END_TAG: &str = "</think>";

static THINK_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<think>(.*?)</think>").unwrap());

/// Models that support the `reasoning_effort` parameter
const REASONING_EFFORT_MODELS: &[&str] = &["glm-5.2", "glm-5.1", "glm-5-turbo"];

/// Check if a model supports reasoning_effort parameter
fn supports_reasoning_effort(model: &str) -> bool {
    REASONING_EFFORT_MODELS
        .iter()
        .any(|m| model.eq_ignore_ascii_case(m) || model.starts_with(&format!("{m}-")))
}

#[derive(Debug, Default)]
struct StreamState {
    in_think: bool,
    pending: String,
}

#[derive(Debug, Clone)]
pub struct GlmTransformer {
    stream_state: Arc<Mutex<StreamState>>,
}

impl Default for GlmTransformer {
    fn default() -> Self {
        Self {
            stream_state: Arc::new(Mutex::new(StreamState::default())),
        }
    }
}

impl GlmTransformer {
    fn extract_thinking(content: &str) -> (String, Option<String>) {
        let mut reasoning = String::new();
        let clean = THINK_REGEX.replace_all(content, |caps: &regex::Captures| {
            if let Some(think) = caps.get(1) {
                if !reasoning.is_empty() {
                    reasoning.push('\n');
                }
                reasoning.push_str(think.as_str().trim());
            }
            ""
        });
        let reasoning_opt = if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        };
        (clean.trim().to_string(), reasoning_opt)
    }

    fn trailing_partial_len(text: &str, token: &str) -> usize {
        let max_len = token.len().saturating_sub(1).min(text.len());
        for len in (1..=max_len).rev() {
            let Some(suffix) = text.get(text.len() - len..) else {
                continue;
            };
            if token.starts_with(suffix) {
                return len;
            }
        }
        0
    }

    fn extract_thinking_streaming(&self, chunk: &str) -> (String, Option<String>) {
        let mut state = self
            .stream_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut input = std::mem::take(&mut state.pending);
        input.push_str(chunk);

        let mut clean = String::new();
        let mut reasoning = String::new();
        let mut cursor = 0;

        while cursor < input.len() {
            if state.in_think {
                if let Some(rel_end) = input[cursor..].find(THINK_END_TAG) {
                    let end = cursor + rel_end;
                    reasoning.push_str(&input[cursor..end]);
                    cursor = end + THINK_END_TAG.len();
                    state.in_think = false;
                } else {
                    let tail = &input[cursor..];
                    let keep = Self::trailing_partial_len(tail, THINK_END_TAG);
                    let emit_until = tail.len().saturating_sub(keep);
                    reasoning.push_str(&tail[..emit_until]);
                    if keep > 0 {
                        state.pending.push_str(&tail[emit_until..]);
                    }
                    break;
                }
            } else if let Some(rel_start) = input[cursor..].find(THINK_START_TAG) {
                let start = cursor + rel_start;
                clean.push_str(&input[cursor..start]);
                cursor = start + THINK_START_TAG.len();
                state.in_think = true;
            } else {
                let tail = &input[cursor..];
                let keep = Self::trailing_partial_len(tail, THINK_START_TAG);
                let emit_until = tail.len().saturating_sub(keep);
                clean.push_str(&tail[..emit_until]);
                if keep > 0 {
                    state.pending.push_str(&tail[emit_until..]);
                }
                break;
            }
        }

        let reasoning_opt = if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        };
        (clean, reasoning_opt)
    }

    fn process_parent(&self, parent: &mut Value, is_streaming_delta: bool) {
        let Some(obj) = parent.as_object_mut() else {
            return;
        };

        // Modern GLM-5.2+ API returns native reasoning_content field.
        // Preserve both content and reasoning_content as-is.
        if obj.contains_key("reasoning_content") {
            trace!("Modern GLM API detected - preserving content and reasoning_content");
            return;
        }

        let Some(content_val) = obj.get("content").cloned() else {
            return;
        };

        let (new_content, reasoning) = match content_val {
            Value::String(s) => {
                let (clean_text, reasoning_opt) = if is_streaming_delta {
                    self.extract_thinking_streaming(&s)
                } else {
                    Self::extract_thinking(&s)
                };
                (Some(Value::String(clean_text)), reasoning_opt)
            }
            Value::Array(blocks) => {
                let mut all_reasoning = Vec::new();
                let new_blocks: Vec<Value> = blocks
                    .into_iter()
                    .map(|mut block| {
                        let Some(text_str) = block.get("text").and_then(Value::as_str) else {
                            return block;
                        };
                        let (clean_text, reasoning_opt) = Self::extract_thinking(text_str);
                        if let Some(r) = reasoning_opt {
                            all_reasoning.push(r);
                        }
                        if let Some(block_obj) = block.as_object_mut() {
                            block_obj.insert("text".to_string(), Value::String(clean_text));
                        }
                        block
                    })
                    .collect();

                let reasoning_opt = if all_reasoning.is_empty() {
                    None
                } else {
                    Some(all_reasoning.join("\n"))
                };
                (Some(Value::Array(new_blocks)), reasoning_opt)
            }
            _ => (None, None),
        };

        if let Some(content) = new_content {
            obj.insert("content".to_string(), content);
        }
        if let Some(extracted) = reasoning {
            let merged_reasoning = match obj.get("reasoning_content").and_then(Value::as_str) {
                Some(existing) if !existing.is_empty() => format!("{existing}\n{extracted}"),
                _ => extracted,
            };
            obj.insert(
                "reasoning_content".to_string(),
                Value::String(merged_reasoning),
            );
        }
    }
}

impl Transformer for GlmTransformer {
    fn name(&self) -> &str {
        "glm"
    }

    fn transform_request(&self, mut request: Value) -> Result<Value> {
        let Some(obj) = request.as_object_mut() else {
            return Ok(request);
        };

        // Clone model name to avoid borrow issues during mutation
        let model = obj
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();

        // Inject reasoning_effort for supported models if not already set
        if supports_reasoning_effort(&model) && !obj.contains_key("reasoning_effort") {
            // Default to "medium" effort for balanced performance
            obj.insert(
                "reasoning_effort".to_string(),
                Value::String("medium".to_string()),
            );
            trace!("Injected reasoning_effort=medium for model {}", model);
        }

        // Strip Anthropic-specific passthrough fields
        obj.remove("metadata");
        obj.remove("anthropic-beta");
        obj.remove("anthropic-version");
        obj.remove("anthropic_version");

        trace!("GLM request transformed for model {}", model);
        Ok(request)
    }

    fn transform_response(&self, mut response: Value) -> Result<Value> {
        trace!(response = ?response, "Starting GLM transform");

        if let Some(choices) = response.get_mut("choices").and_then(|c| c.as_array_mut()) {
            for choice in choices {
                if let Some(message) = choice.get_mut("message") {
                    self.process_parent(message, false);
                }
                if let Some(delta) = choice.get_mut("delta") {
                    self.process_parent(delta, true);
                }
            }
        } else {
            self.process_parent(&mut response, false);
        }

        trace!(response = ?response, "Finished GLM transform");
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_single_think_block() {
        let transformer = GlmTransformer::default();
        let response = json!({
            "content": "Here is the answer. <think>I am thinking.</think>"
        });

        let transformed = transformer.transform_response(response).unwrap();
        assert_eq!(transformed["content"], "Here is the answer.");
        assert_eq!(transformed["reasoning_content"], "I am thinking.");
    }

    #[test]
    fn extracts_multiple_think_blocks() {
        let transformer = GlmTransformer::default();
        let response = json!({
            "content": "Answer. <think>Step 1.</think> More answer. <think>Step 2.</think>"
        });

        let transformed = transformer.transform_response(response).unwrap();
        assert_eq!(transformed["content"], "Answer.  More answer.");
        assert_eq!(transformed["reasoning_content"], "Step 1.\nStep 2.");
    }

    #[test]
    fn passthrough_when_no_think_tags() {
        let transformer = GlmTransformer::default();
        let response = json!({
            "content": "Just a plain response."
        });
        let original_response = response.clone();

        let transformed = transformer.transform_response(response).unwrap();
        assert_eq!(transformed["content"], original_response["content"]);
        assert!(transformed.get("reasoning_content").is_none());
    }

    #[test]
    fn handles_anthropic_style_content_array() {
        let transformer = GlmTransformer::default();
        let response = json!({
            "content": [
                {
                    "type": "text",
                    "text": "Here is the answer. <think>I am thinking.</think>"
                }
            ]
        });

        let transformed = transformer.transform_response(response).unwrap();
        assert_eq!(transformed["content"][0]["text"], "Here is the answer.");
        assert_eq!(transformed["reasoning_content"], "I am thinking.");
    }

    #[test]
    fn handles_streaming_delta() {
        let transformer = GlmTransformer::default();
        let response = json!({
            "choices": [
                {
                    "delta": {
                        "content": " and more...<think>...and more thinking</think>"
                    }
                }
            ]
        });

        let transformed = transformer.transform_response(response).unwrap();
        let delta = &transformed["choices"][0]["delta"];
        assert_eq!(delta["content"], " and more...");
        assert_eq!(delta["reasoning_content"], "...and more thinking");
    }

    #[test]
    fn handles_multiline_think_block() {
        let transformer = GlmTransformer::default();
        let content = "The answer is 42.\n<think>First, I need to understand the question.
It is the a-team question of life, the universe, and everything.
Then, I recall the answer from my knowledge base.</think>";
        let response = json!({ "content": content });

        let transformed = transformer.transform_response(response).unwrap();
        assert_eq!(transformed["content"], "The answer is 42.");
        assert_eq!(
            transformed["reasoning_content"],
            "First, I need to understand the question.\nIt is the a-team question of life, the universe, and everything.\nThen, I recall the answer from my knowledge base."
        );
    }

    #[test]
    fn streaming_accumulates_partial_tags_across_chunks() {
        let transformer = GlmTransformer::default();

        let chunk_1 = json!({
            "choices": [{
                "delta": {
                    "content": "Before <thi"
                }
            }]
        });
        let transformed_1 = transformer.transform_response(chunk_1).unwrap();
        let delta_1 = &transformed_1["choices"][0]["delta"];
        assert_eq!(delta_1["content"], "Before ");
        assert!(delta_1.get("reasoning_content").is_none());

        let chunk_2 = json!({
            "choices": [{
                "delta": {
                    "content": "nk>reason"
                }
            }]
        });
        let transformed_2 = transformer.transform_response(chunk_2).unwrap();
        let delta_2 = &transformed_2["choices"][0]["delta"];
        assert_eq!(delta_2["content"], "");
        assert_eq!(delta_2["reasoning_content"], "reason");

        let chunk_3 = json!({
            "choices": [{
                "delta": {
                    "content": "ing</think> after"
                }
            }]
        });
        let transformed_3 = transformer.transform_response(chunk_3).unwrap();
        let delta_3 = &transformed_3["choices"][0]["delta"];
        assert_eq!(delta_3["content"], " after");
        assert_eq!(delta_3["reasoning_content"], "ing");
    }

    #[test]
    fn preserves_modern_glm_native_reasoning_content() {
        let transformer = GlmTransformer::default();
        // Modern GLM-5.2 returns native reasoning_content without think tags
        let response = json!({
            "choices": [{
                "message": {
                    "content": "2 + 2 = 4",
                    "reasoning_content": "First, I need to understand the question.\nThen, I calculate the result.",
                    "role": "assistant"
                }
            }]
        });

        let transformed = transformer.transform_response(response).unwrap();
        let message = &transformed["choices"][0]["message"];
        // Both fields should be preserved as-is
        assert_eq!(message["content"], "2 + 2 = 4");
        assert_eq!(
            message["reasoning_content"],
            "First, I need to understand the question.\nThen, I calculate the result."
        );
    }

    #[test]
    fn preserves_modern_glm_with_only_reasoning_content() {
        let transformer = GlmTransformer::default();
        // Edge case: response with reasoning_content but empty content
        let response = json!({
            "choices": [{
                "message": {
                    "content": "",
                    "reasoning_content": "This is the reasoning.",
                    "role": "assistant"
                }
            }]
        });

        let transformed = transformer.transform_response(response).unwrap();
        let message = &transformed["choices"][0]["message"];
        assert_eq!(message["content"], "");
        assert_eq!(message["reasoning_content"], "This is the reasoning.");
    }
}
