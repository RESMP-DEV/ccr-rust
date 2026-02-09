use serde_json::json;

fn main() {
    let content = json!([
        {
            "type": "thinking",
            "thinking": "Let me think about this..."
        },
        {
            "type": "text",
            "text": "Final answer."
        }
    ]);
    
    println!("Content: {}", content);
    println!("This simulates what normalize_message_content would do.");
    println!("In normalize_message_content, thinking blocks are wrapped in <thinking> tags.");
    println!("In codex.rs serialize_response, thinking blocks go to reasoning_content field.");
}
