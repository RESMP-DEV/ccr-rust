use super::*;
use serde_json::json;

#[test]
fn rejects_non_object_responses_reasoning() {
    let request = json!({
        "model": "auto",
        "input": "Think",
        "reasoning": "high"
    });

    let error = responses_request_to_openai_chat_request(&request).unwrap_err();

    assert_eq!(error, "responses request 'reasoning' must be an object");
}

#[test]
fn drops_null_previous_response_id() {
    let request = json!({
        "model": "auto",
        "input": "Continue",
        "previous_response_id": null
    });

    let converted = responses_request_to_openai_chat_request(&request).unwrap();

    assert!(converted.get("previous_response_id").is_none());
}

#[test]
fn continuation_fallback_omits_identity_metadata() {
    let item = json!({
        "type": "mcp_approval_response",
        "approval_request_id": "approval_1",
        "approve": true
    });

    assert_eq!(normalize_continuation_output(&item), "{\"approve\":true}");
}

#[test]
fn input_image_accepts_string_and_object_forms() {
    let content = json!([
        {"type": "input_text", "text": "see:"},
        {"type": "input_image", "image_url": "data:image/png;base64,AAAA"},
        {"type": "input_image", "image_url": {"url": "https://example.test/x.png", "detail": "low"}}
    ]);
    let converted = responses_content_to_openai_content(&content);
    let blocks = converted.as_array().unwrap();
    assert_eq!(blocks[0]["type"], "text");
    assert_eq!(blocks[1]["type"], "image_url");
    assert_eq!(blocks[1]["image_url"]["url"], "data:image/png;base64,AAAA");
    assert_eq!(blocks[2]["type"], "image_url");
    assert_eq!(blocks[2]["image_url"]["url"], "https://example.test/x.png");
}

#[test]
fn input_image_without_url_is_dropped() {
    let content = json!([
        {"type": "input_text", "text": "hi"},
        {"type": "input_image"}
    ]);
    let converted = responses_content_to_openai_content(&content);
    // The surviving single text block collapses to a plain string.
    assert_eq!(converted, json!("hi"));
}

#[test]
fn chat_refusal_serializes_as_responses_refusal_block() {
    let openai = json!({
        "id": "resp_refusal",
        "object": "chat.completion",
        "created": 42,
        "model": "muse",
        "response_status": "incomplete",
        "incomplete_details": {"reason": "content_filter"},
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "refusal": "I cannot help with that."
            },
            "finish_reason": "content_filter"
        }]
    });

    let response = openai_chat_completion_to_responses_json(&openai, None);

    assert_eq!(
        response["output"][0]["content"].as_array().unwrap().len(),
        1
    );
    assert_eq!(response["output"][0]["content"][0]["type"], "refusal");
    assert_eq!(
        response["output"][0]["content"][0]["refusal"],
        "I cannot help with that."
    );
    assert_eq!(response["status"], "incomplete");
    assert_eq!(
        response["incomplete_details"],
        json!({"reason": "content_filter"})
    );
}
