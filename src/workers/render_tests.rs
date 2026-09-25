use super::write;
use serde_json::{json, Value};

fn rendered(value: &Value) -> String {
    let mut output = Vec::new();
    write(&mut output, value).unwrap();
    String::from_utf8(output).unwrap()
}

fn event(id: &str, exit_code: i64) -> Value {
    json!({
        "cursor": 42,
        "event": "item.completed",
        "item_id": id,
        "summary": {"text": format!("summary {id}"), "truncated": false},
        "exit_code": exit_code
    })
}

#[test]
fn renders_list_headlines_and_omission_footer() {
    let value = json!({
        "runs": [
            {"run_id": "run-c", "outcome": "completed", "needs_attention": false,
             "commands_completed": 2, "failed_commands": 0, "failed_items": 0, "file_changes": 1,
             "usage": {"input_tokens": 10, "output_tokens": 3}},
            {"run_id": "run-b", "outcome": "failed", "needs_attention": true,
             "commands_completed": 1, "failed_commands": 1, "failed_items": 0, "file_changes": 0,
             "usage": {"input_tokens": 4, "output_tokens": 1}}
        ],
        "omitted_runs": 2
    });
    let output = rendered(&value);
    assert!(output.contains(
        "run-c  completed (unverified)  commands=2 failed=0 failed_items=0 files=1  tokens=10/3"
    ));
    assert!(output.contains("run-b  failed (unverified)  commands=1 failed=1 failed_items=0 files=0  tokens=4/1  ATTENTION"));
    assert!(output.contains("2 runs shown; 2 omitted. Use workers show RUN for evidence."));
}

#[test]
fn renders_event_pages_and_stream_footer() {
    let value = json!({
        "events": [event("one", 0), event("two", 2)],
        "next_cursor": 99,
        "has_more": true,
        "trace": {"partial_line": true, "malformed_lines": 3, "oversized_lines": 4}
    });
    let output = rendered(&value);
    assert!(output.contains("  @42 item.completed one summary one"));
    assert!(output.contains("    exit=0"));
    assert!(output.contains("  @42 item.completed two summary two"));
    assert!(output.contains("    exit=2"));
    assert!(output.contains("next_cursor=99 has_more=true partial_line=true skipped=3/4"));
}

#[test]
fn renders_detail_truncation_hint() {
    let value = json!({
        "cursor": 42,
        "event": "item.completed",
        "item_id": "chosen",
        "summary": {"text": "chosen command", "truncated": false},
        "detail": {"text": "αβγδεž漢🙂🙂🙂", "truncated": true}
    });
    let output = rendered(&value);
    assert!(output.contains("  @42 item.completed chosen chosen command"));
    assert!(output.contains("αβγδεž漢🙂🙂🙂"));
    assert!(output.contains("[truncated; increase --max-chars, maximum 16000]"));
}

#[test]
fn human_metadata_cannot_inject_lines_or_terminal_controls() {
    let value = json!({
        "run_id": "human\nFORGED_RUN",
        "outcome": "failed",
        "needs_attention": true,
        "model_requested": "model\nFORGED_MODEL",
        "commands_completed": 1,
        "failed_commands": 0,
        "failed_items": 0,
        "file_changes": 0,
        "usage": {"input_tokens": 1, "cached_input_tokens": 0, "output_tokens": 2, "reasoning_output_tokens": 0, "reported_turns": 1},
        "active_items": [],
        "failures": [],
        "recent": [{
            "cursor": 9, "event": "item.completed", "item_id": "item\nFORGED_ID",
            "summary": {"text": "safe\nFORGED_SUMMARY\u{1b}\t", "truncated": false}, "exit_code": 0
        }],
        "warnings": ["one\nFORGED_WARNING\tmore\u{1b}"],
        "final_report": {"text": "claim\nreport body", "truncated": false},
        "trace": {"file_bytes": 18, "next_cursor": 18},
        "stderr_bytes": 0
    });
    let output = rendered(&value);
    assert!(output.contains("ATTENTION"));
    assert!(output.contains("safe FORGED_SUMMARY\u{FFFD} "));
    assert!(output.contains("one FORGED_WARNING more\u{FFFD}"));
    assert!(output.contains("claim\nreport body"));
    assert!(!output.contains("\nFORGED_"));
    assert!(!output.contains('\u{1b}'));
    assert!(!output.contains('\t'));
}

#[test]
fn missing_human_fields_render_as_unknown_or_zero() {
    let value = json!({
        "run_id": "minimal",
        "outcome": "receipt_pending",
        "commands_completed": 0,
        "failed_commands": 0,
        "failed_items": 0,
        "file_changes": 0,
        "usage": {"input_tokens": 0, "output_tokens": 0},
        "warnings": [],
        "trace": {"file_bytes": 0, "next_cursor": 0},
        "stderr_bytes": 0
    });
    let output = rendered(&value);
    assert!(output.contains("minimal  receipt_pending (unverified)  commands=0 failed=0 failed_items=0 files=0  tokens=0/0"));
    assert!(output.contains("Requested model: unknown (provider not verified by saved trace)"));
    assert!(output.contains("Trace: 0 bytes; cursor=0; stderr=0 bytes (not included)"));
}
