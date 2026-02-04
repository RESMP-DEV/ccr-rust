//! Think tag transformer module.
//!
//! Strips thinking/reasoning tags from response text content.
//! Removes <think>, <thinking>, and <reasoning> blocks and their content.

use crate::transformer::Transformer;
use anyhow::Result;
use regex::Regex;
use serde_json::Value;

use lazy_static::lazy_static;

lazy_static! {
    // Regex crate doesn't support backreferences, so use alternation
    static ref THINK_TAG_RE: Regex = Regex::new(
        r"(?s)<think>.*?</think>|<thinking>.*?</thinking>|<reasoning>.*?</reasoning>"
    ).unwrap();
}

/// Think tag transformer.
///
/// Strips thinking/reasoning tags from response text content.
#[derive(Debug, Clone)]
pub struct ThinkTagTransformer;

impl Transformer for ThinkTagTransformer {
    fn name(&self) -> &str {
        "thinktag"
    }

    fn transform_response(&self, mut response: Value) -> Result<Value> {
        if let Some(content) = response.get_mut("content") {
            if let Some(arr) = content.as_array_mut() {
                for block in arr {
                    if let Some(text) = block.get_mut("text") {
                        if let Some(s) = text.as_str() {
                            let stripped = THINK_TAG_RE.replace_all(s, "");
                            *text = Value::String(stripped.trim().to_string());
                        }
                    }
                }
            }
        }
        Ok(response)
    }
}
