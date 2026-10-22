use super::*;
use serde_json::json;

#[test]
fn agent_message_items_convert_to_assistant_messages() {
    let body = json!({
        "model": "m",
        "input": [{
            "type": "agent_message",
            "author": "worker",
            "recipient": "user",
            "content": [{"type": "output_text", "text": "task done"}]
        }]
    });
    let request =
        responses_request_to_openai_chat_request(&body).expect("agent message must convert");
    let messages = request["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(
        messages[0]["content"],
        "Message from agent 'worker' to 'user':"
    );
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"], "task done");
}

#[test]
fn consecutive_agent_messages_merge_into_one_assistant_turn() {
    let body = json!({
        "model": "m",
        "input": [
            {"type": "agent_message", "author": "worker", "recipient": "user",
             "content": [{"type": "output_text", "text": "first"}]},
            {"type": "agent_message", "author": "worker", "recipient": "user",
             "content": [{"type": "output_text", "text": "second"}]}
        ]
    });
    let request = responses_request_to_openai_chat_request(&body).unwrap();
    let messages = request["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"], "first\n\nsecond");
}

#[test]
fn merging_into_array_content_keeps_existing_blocks() {
    let image_block = json!({
        "type": "image_url",
        "image_url": {"url": "data:image/png;base64,AAAA"}
    });
    let body = json!({
        "model": "m",
        "input": [
            {"type": "agent_message", "author": "w", "recipient": "user",
             "content": [
                 {"type": "input_image", "image_url": "data:image/png;base64,AAAA"},
                 {"type": "output_text", "text": "screenshot"}
             ]},
            {"type": "agent_message", "author": "w", "recipient": "user",
             "content": [{"type": "output_text", "text": "follow-up"}]}
        ]
    });
    let request = responses_request_to_openai_chat_request(&body).unwrap();
    let messages = request["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1]["role"], "assistant");
    let blocks = messages[1]["content"].as_array().expect("array content");
    assert_eq!(
        blocks.len(),
        3,
        "must keep image, first text, and appended text"
    );
    assert_eq!(blocks[0], image_block);
    assert_eq!(blocks[1]["text"], "screenshot");
    assert_eq!(blocks[2]["text"], "follow-up");
}

#[test]
fn agent_message_mixed_input_preserves_order() {
    let body = json!({
        "model": "m",
        "input": [
            {"type": "message", "role": "user", "content": "run it"},
            {"type": "agent_message", "author": "worker", "recipient": "user",
             "content": [{"type": "output_text", "text": "ran"}]}
        ]
    });
    let request = responses_request_to_openai_chat_request(&body).unwrap();
    assert_eq!(request["messages"].as_array().unwrap().len(), 2);
    assert_eq!(request["messages"].as_array().unwrap()[0]["role"], "user");
    assert_eq!(
        request["messages"].as_array().unwrap()[1]["role"],
        "assistant"
    );
    assert_eq!(request["messages"].as_array().unwrap()[1]["content"], "ran");
}
