//! Integration tests for unified reasoning normalization.

use ccr_rust::transform::{
    DeepSeekTransformer, GlmTransformer, KimiTransformer, MinimaxTransformer,
};
use serde_json::json;

mod common {
    use ccr_rust::transformer::Transformer;
    use serde_json::Value;

    pub fn apply_response_transform(transformer: &dyn Transformer, response: Value) -> Value {
        transformer
            .transform_response(response)
            .expect("response transform should succeed")
    }

    pub fn message(response: &Value) -> &Value {
        response
            .get("choices")
            .and_then(|choices| choices.as_array())
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .expect("response should include choices[0].message")
    }
}

/// Test DeepSeek reasoner preserves reasoning_content
#[test]
fn test_reasoning_normalization_deepseek_reasoning_content() {
    let response = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "The answer is 42",
                "reasoning_content": "Let me think step by step..."
            }
        }]
    });

    let transformer = DeepSeekTransformer;
    let transformed = common::apply_response_transform(&transformer, response);
    let message = common::message(&transformed);

    assert_eq!(message["content"], "The answer is 42");
    assert_eq!(message["reasoning_content"], "Let me think step by step...");
}

/// Test Minimax reasoning_details -> reasoning_content mapping
#[test]
fn test_reasoning_normalization_minimax_reasoning_mapping() {
    let response = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "The answer is 42",
                "reasoning_details": "Thinking through the problem..."
            }
        }]
    });

    let transformer = MinimaxTransformer;
    let transformed = common::apply_response_transform(&transformer, response);
    let message = common::message(&transformed);

    assert_eq!(message["content"], "The answer is 42");
    assert_eq!(
        message["reasoning_content"],
        "Thinking through the problem..."
    );
    assert!(message.get("reasoning_details").is_none());
}

/// Test GLM think tag extraction
#[test]
fn test_reasoning_normalization_glm_think_extraction() {
    let response = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "<think>Step 1: analyze</think>The answer is 42"
            }
        }]
    });

    let transformer = GlmTransformer::default();
    let transformed = common::apply_response_transform(&transformer, response);
    let message = common::message(&transformed);

    assert_eq!(message["content"], "The answer is 42");
    assert_eq!(message["reasoning_content"], "Step 1: analyze");
}

/// Test Kimi Unicode token extraction
#[test]
fn test_reasoning_normalization_kimi_unicode_extraction() {
    let response = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "◁think▷Reasoning here◁/think▷The answer is 42"
            }
        }]
    });

    let transformer = KimiTransformer;
    let transformed = common::apply_response_transform(&transformer, response);
    let message = common::message(&transformed);

    assert_eq!(message["content"], "The answer is 42");
    assert_eq!(message["reasoning_content"], "Reasoning here");
}
