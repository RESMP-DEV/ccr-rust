use super::*;
use serde_json::json;

#[test]
fn signed_reasoning_and_function_call_form_one_assistant_turn() {
    let encrypted = serde_json::to_string(&vec![json!({
        "thinking": "Select the tool.",
        "signature": "sig-1"
    })])
    .unwrap();
    let request = json!({
        "model": "m",
        "input": [
            {"type": "message", "role": "user", "content": "weather?"},
            {
                "type": "reasoning",
                "id": "rs_prior",
                "content": [{"type": "reasoning_text", "text": "Select the tool."}],
                "encrypted_content": encrypted
            },
            {
                "type": "function_call",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": "{}"
            },
            {
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "sunny"
            }
        ]
    });

    let converted = responses_request_to_openai_chat_request(&request).unwrap();
    let messages = converted["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(
        messages[1]["reasoning_blocks"][0]["thinking"],
        "Select the tool."
    );
    assert_eq!(messages[1]["reasoning_blocks"][0]["signature"], "sig-1");
    assert_eq!(messages[1]["tool_calls"][0]["id"], "call_1");
    assert_eq!(messages[2]["role"], "tool");
}

#[test]
fn signed_reasoning_blocks_serialize_as_replayable_responses_items() {
    let blocks = vec![json!({
        "thinking": "Select the tool.",
        "signature": "sig-1"
    })];
    let openai = json!({
        "id": "resp_signed",
        "object": "chat.completion",
        "created": 42,
        "model": "MiniMax-M3.1-Flash",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "",
                "reasoning_blocks": blocks,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "get_weather", "arguments": "{}"}
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });

    let response = openai_chat_completion_to_responses_json(&openai, None);
    let reasoning = &response["output"][0];
    assert_eq!(reasoning["type"], "reasoning");
    assert_eq!(reasoning["content"][0]["text"], "Select the tool.");
    let replay_blocks: Vec<serde_json::Value> =
        serde_json::from_str(reasoning["encrypted_content"].as_str().unwrap()).unwrap();
    assert_eq!(replay_blocks[0]["signature"], "sig-1");
    assert_eq!(response["output"][2]["type"], "function_call");
}
