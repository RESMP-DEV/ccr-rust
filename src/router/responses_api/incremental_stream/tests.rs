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

#[test]
fn signed_streaming_reasoning_is_replayable() {
    let mut converter = ResponsesStreamConverter::new(None);
    let frames = [
        serde_json::json!({
            "id": "resp_signed",
            "choices": [{
                "delta": {"reasoning_block_index": 0}
            }]
        }),
        serde_json::json!({
            "id": "resp_signed",
            "choices": [{
                "delta": {"reasoning_content": "Select the tool."}
            }]
        }),
        serde_json::json!({
            "id": "resp_signed",
            "choices": [{
                "delta": {"reasoning_signature": "sig-1"}
            }]
        }),
        serde_json::json!({
            "id": "resp_signed",
            "choices": [{
                "delta": {"reasoning_block_index": 2}
            }]
        }),
        serde_json::json!({
            "id": "resp_signed",
            "choices": [{
                "delta": {"reasoning_content": "Use the result."}
            }]
        }),
        serde_json::json!({
            "id": "resp_signed",
            "choices": [{
                "delta": {"reasoning_signature": "sig-2"}
            }]
        }),
    ];
    for frame in frames {
        converter.push_frame(None, &frame.to_string());
    }
    let output = converter.finish();

    let mut done_item = None;
    for line in output.lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let event: serde_json::Value = serde_json::from_str(data).unwrap();
        if event["type"] == "response.output_item.done" {
            done_item = Some(event["item"].clone());
        }
    }
    let item = done_item.expect("reasoning output item should complete");
    assert_eq!(item["type"], "reasoning");
    assert_eq!(
        item["content"][0]["text"],
        "Select the tool.\n\nUse the result."
    );
    let blocks: Vec<serde_json::Value> =
        serde_json::from_str(item["encrypted_content"].as_str().unwrap()).unwrap();
    assert_eq!(blocks[0]["thinking"], "Select the tool.");
    assert_eq!(blocks[0]["signature"], "sig-1");
    assert_eq!(blocks[1]["thinking"], "Use the result.");
    assert_eq!(blocks[1]["signature"], "sig-2");
}
