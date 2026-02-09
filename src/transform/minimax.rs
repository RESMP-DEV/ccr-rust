//! Minimax M2.1 API transformer.
//!
//! Handles Minimax-specific request/response transformations:
//! - Request: add `reasoning_split: true`
//! - Request: strip Anthropic-specific passthrough fields if present
//! - Response: map `reasoning_details` -> `reasoning_content`

use crate::transformer::Transformer;
use anyhow::Result;
use serde_json::Value;
use tracing::trace;

#[derive(Debug, Clone)]
pub struct MinimaxTransformer;

impl Transformer for MinimaxTransformer {
    fn name(&self) -> &str {
        "minimax"
    }

    fn transform_request(&self, mut request: Value) -> Result<Value> {
        if let Some(obj) = request.as_object_mut() {
            // Enable reasoning_split for structured reasoning output
            obj.insert("reasoning_split".to_string(), Value::Bool(true));

            // Strip Anthropic-specific passthrough fields if present.
            // These may appear if upstream clients forward Anthropic payloads/headers.
            obj.remove("metadata");
            obj.remove("anthropic-beta");
            obj.remove("anthropic-version");
            obj.remove("anthropic_version");
        }

        trace!("Minimax request transformed");
        Ok(request)
    }

    fn transform_response(&self, mut response: Value) -> Result<Value> {
        // Map reasoning_details -> reasoning_content in choices
        if let Some(choices) = response.get_mut("choices") {
            if let Some(choices_array) = choices.as_array_mut() {
                for choice in choices_array {
                    // Handle message (non-streaming)
                    if let Some(message) = choice.get_mut("message") {
                        if let Some(obj) = message.as_object_mut() {
                            if let Some(reasoning) = obj.remove("reasoning_details") {
                                obj.insert("reasoning_content".to_string(), reasoning);
                            }
                        }
                    }
                    // Handle delta (streaming)
                    if let Some(delta) = choice.get_mut("delta") {
                        if let Some(obj) = delta.as_object_mut() {
                            if let Some(reasoning) = obj.remove("reasoning_details") {
                                obj.insert("reasoning_content".to_string(), reasoning);
                            }
                        }
                    }
                }
            }
        }
        trace!("Minimax response transformed");
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_transform_request_adds_reasoning_split() {
        let transformer = MinimaxTransformer;
        let request = json!({
            "model": "minimax-m2.1",
            "messages": [{"role": "user", "content": "Hello"}],
            "metadata": {"user_id": "abc"},
            "anthropic-beta": "tools-2024-04-04",
            "anthropic-version": "2023-06-01",
            "anthropic_version": "2023-06-01"
        });

        let transformed_request = transformer.transform_request(request).unwrap();
        assert_eq!(transformed_request["reasoning_split"], json!(true));
        assert!(transformed_request.get("metadata").is_none());
        assert!(transformed_request.get("anthropic-beta").is_none());
        assert!(transformed_request.get("anthropic-version").is_none());
        assert!(transformed_request.get("anthropic_version").is_none());
    }

    #[test]
    fn test_transform_response_maps_reasoning_details() {
        let transformer = MinimaxTransformer;
        let response = json!({
            "choices": [{
                "message": {
                    "reasoning_details": "Thinking..."
                }
            }]
        });

        let transformed_response = transformer.transform_response(response).unwrap();
        let message = &transformed_response["choices"][0]["message"];
        assert!(message.get("reasoning_details").is_none());
        assert_eq!(message["reasoning_content"], json!("Thinking..."));
    }

    #[test]
    fn test_transform_streaming_response_maps_reasoning_details() {
        let transformer = MinimaxTransformer;
        let response = json!({
            "choices": [{
                "delta": {
                    "reasoning_details": "Still thinking..."
                }
            }]
        });

        let transformed_response = transformer.transform_response(response).unwrap();
        let delta = &transformed_response["choices"][0]["delta"];
        assert!(delta.get("reasoning_details").is_none());
        assert_eq!(delta["reasoning_content"], json!("Still thinking..."));
    }

    #[test]
    fn test_transform_response_no_op_if_no_reasoning() {
        let transformer = MinimaxTransformer;
        let response = json!({
            "choices": [{
                "message": {
                    "content": "Hello there"
                }
            }]
        });
        let original_response = response.clone();

        let transformed_response = transformer.transform_response(response).unwrap();
        assert_eq!(transformed_response, original_response);
    }
}
