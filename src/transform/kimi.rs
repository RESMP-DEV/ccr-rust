//! Kimi K2 (Moonshot) API transformer.
//!
//! This transformer extracts Unicode think tokens (◁think▷...◁/think▷)
//! to the reasoning_content field.

use crate::transformer::Transformer;
use anyhow::Result;
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;
use tracing::trace;

static KIMI_THINK_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)◁think▷(.*?)◁/think▷").unwrap());

/// Kimi K2 (Moonshot) API transformer.
#[derive(Debug, Clone, Default)]
pub struct KimiTransformer;

impl KimiTransformer {
    fn extract_thinking(content: &str) -> (String, Option<String>) {
        let mut reasoning = String::new();
        let clean = KIMI_THINK_REGEX.replace_all(content, |caps: &regex::Captures| {
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
}

impl Transformer for KimiTransformer {
    fn name(&self) -> &str {
        "kimi"
    }

    fn transform_response(&self, mut response: Value) -> Result<Value> {
        trace!(response = ?response, "Starting Kimi transform");

        fn process_parent(parent: &mut Value) {
            let (new_content, reasoning) = if let Some(content_val) = parent.get("content") {
                match content_val {
                    Value::String(s) => {
                        let (clean_text, reasoning_opt) = KimiTransformer::extract_thinking(s);
                        (Some(Value::String(clean_text)), reasoning_opt)
                    }
                    Value::Array(blocks) => {
                        let mut all_reasoning = String::new();
                        let new_blocks: Vec<Value> = blocks
                            .iter()
                            .map(|block| {
                                let mut new_block = block.clone();
                                if let Some(text_val) = new_block.get_mut("text") {
                                    if let Some(text_str) = text_val.as_str() {
                                        let (clean_text, reasoning_opt) =
                                            KimiTransformer::extract_thinking(text_str);
                                        if let Some(r) = reasoning_opt {
                                            if !all_reasoning.is_empty() {
                                                all_reasoning.push('\n');
                                            }
                                            all_reasoning.push_str(&r);
                                        }
                                        *text_val = Value::String(clean_text);
                                    }
                                }
                                new_block
                            })
                            .collect();

                        let reasoning_opt = if all_reasoning.is_empty() {
                            None
                        } else {
                            Some(all_reasoning)
                        };
                        (Some(Value::Array(new_blocks)), reasoning_opt)
                    }
                    _ => (None, None),
                }
            } else {
                (None, None)
            };

            if let Some(obj) = parent.as_object_mut() {
                if let Some(content) = new_content {
                    obj.insert("content".to_string(), content);
                }
                if let Some(r) = reasoning {
                    obj.insert("reasoning_content".to_string(), Value::String(r));
                }
            }
        }

        if let Some(choices) = response.get_mut("choices").and_then(|c| c.as_array_mut()) {
            for choice in choices {
                if let Some(message) = choice.get_mut("message") {
                    process_parent(message);
                }
                if let Some(delta) = choice.get_mut("delta") {
                    process_parent(delta);
                }
            }
        } else {
            process_parent(&mut response);
        }

        trace!(response = ?response, "Finished Kimi transform");
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_unicode_think_blocks() {
        let transformer = KimiTransformer;
        let response = json!({
            "choices": [{
                "message": {
                    "content": "◁think▷Let me think about this.\n\nMulti-line reasoning.◁/think▷This is the answer."
                }
            }]
        });

        let result = transformer.transform_response(response).unwrap();
        let message = &result["choices"][0]["message"];

        assert_eq!(message["content"], "This is the answer.");
        assert_eq!(
            message["reasoning_content"],
            "Let me think about this.\n\nMulti-line reasoning."
        );
    }

    #[test]
    fn handles_multibyte_utf8_correctly() {
        let transformer = KimiTransformer;
        let response = json!({
            "choices": [{
                "message": {
                    "content": "◁think▷Chinese: 你好\nEmoji: 🧠\nSpecial: ◁test▷◁/think▷Response"
                }
            }]
        });

        let result = transformer.transform_response(response).unwrap();
        let message = &result["choices"][0]["message"];

        assert_eq!(message["content"], "Response");
        assert_eq!(
            message["reasoning_content"],
            "Chinese: 你好\nEmoji: 🧠\nSpecial: ◁test▷"
        );
    }

    #[test]
    fn passthrough_when_no_tokens() {
        let transformer = KimiTransformer;
        let response = json!({
            "choices": [{
                "message": {
                    "content": "Just a normal answer without thinking tokens."
                }
            }]
        });

        let result = transformer.transform_response(response).unwrap();
        let message = &result["choices"][0]["message"];

        // Should be unchanged
        assert_eq!(
            message["content"],
            "Just a normal answer without thinking tokens."
        );
        assert!(message.get("reasoning_content").is_none());
    }

    #[test]
    fn extracts_multiple_think_blocks() {
        let transformer = KimiTransformer;
        let response = json!({
            "choices": [{
                "message": {
                    "content": "◁think▷First thought◁/think▷Part 1\n◁think▷Second thought◁/think▷Part 2"
                }
            }]
        });

        let result = transformer.transform_response(response).unwrap();
        let message = &result["choices"][0]["message"];

        assert_eq!(message["content"], "Part 1\nPart 2");
        assert_eq!(
            message["reasoning_content"],
            "First thought\nSecond thought"
        );
    }

    #[test]
    fn handles_empty_think_block() {
        let transformer = KimiTransformer;
        let response = json!({
            "choices": [{
                "message": {
                    "content": "◁think▷◁/think▷Answer"
                }
            }]
        });

        let result = transformer.transform_response(response).unwrap();
        let message = &result["choices"][0]["message"];

        assert_eq!(message["content"], "Answer");
        assert!(message.get("reasoning_content").is_none());
    }

    #[test]
    fn handles_streaming_delta() {
        let transformer = KimiTransformer;
        let response = json!({
            "choices": [
                {
                    "delta": {
                        "content": "prelude ◁think▷delta thought◁/think▷final"
                    }
                }
            ]
        });

        let result = transformer.transform_response(response).unwrap();
        let delta = &result["choices"][0]["delta"];
        assert_eq!(delta["content"], "prelude final");
        assert_eq!(delta["reasoning_content"], "delta thought");
    }

    #[test]
    fn handles_anthropic_style_content_array() {
        let transformer = KimiTransformer;
        let response = json!({
            "content": [
                {
                    "type": "text",
                    "text": "answer ◁think▷array reasoning◁/think▷ done"
                }
            ]
        });

        let result = transformer.transform_response(response).unwrap();
        assert_eq!(result["content"][0]["text"], "answer  done");
        assert_eq!(result["reasoning_content"], "array reasoning");
    }
}
