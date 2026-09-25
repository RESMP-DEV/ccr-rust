use std::fs;
use std::path::Path;

use super::{detail, events, list_entry, read_receipt, summarize, text, Usage};
use serde_json::{json, Value};
use tempfile::TempDir;

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn event_offsets(trace: &Path, events: &[Value]) -> (Vec<u64>, Vec<u64>) {
    let mut offset = if trace.exists() {
        fs::metadata(trace).unwrap().len()
    } else {
        0
    };
    let mut starts = Vec::new();
    let mut cursors = Vec::new();
    let mut contents = if trace.exists() {
        fs::read_to_string(trace).unwrap()
    } else {
        String::new()
    };
    for event in events {
        let line = serde_json::to_string(event).unwrap();
        starts.push(offset);
        contents.push_str(&line);
        contents.push('\n');
        offset += line.len() as u64 + 1;
        cursors.push(offset);
    }
    write(trace, &contents);
    (starts, cursors)
}

fn command_event(id: &str, exit_code: i64, status: &str) -> Value {
    json!({
        "type": "item.completed",
        "item": {
            "type": "command_execution",
            "id": id,
            "status": status,
            "command": "checked saved evidence",
            "exit_code": exit_code,
            "aggregated_output": format!("output from {id}")
        }
    })
}

#[test]
fn summarize_counts_failures_usage_and_bounds_final_report() {
    let directory = TempDir::new().unwrap();
    let run = directory.path().join("unicode-run");
    write(
        &run.join("status.json"),
        &json!({
            "outcome": "failed",
            "exit_code": 1,
            "model": "zai,glm-5.3-flashx",
            "errors": ["provider rate limited \u{7}"]
        })
        .to_string(),
    );
    let (_, cursors) = event_offsets(
        &run.join("events.jsonl"),
        &[
            json!({"type": "turn.started"}),
            json!({"type": "item.started", "item": {
                "type": "command_execution", "id": "failed-command"
            }}),
            json!({"type": "item.completed", "item": {
                "type": "command_execution",
                "id": "failed-command",
                "status": "failed",
                "command": "bell \u{7} and\nnewline",
                "exit_code": 2,
                "aggregated_output": "private tool output"
            }}),
            json!({"type": "turn.completed", "usage": {
                "input_tokens": 11,
                "cached_input_tokens": 3,
                "output_tokens": 7,
                "reasoning_output_tokens": 2
            }}),
            json!({"type": "error", "error": "upstream unavailable"}),
        ],
    );
    write(&run.join("final.txt"), "αβγδεž漢🙂🙂🙂🙂 tail");

    let shown = summarize(&run, 10).unwrap();
    assert_eq!(shown["schema_version"], 1);
    assert_eq!(shown["outcome"], "failed");
    assert_eq!(shown["needs_attention"], true);
    assert_eq!(shown["model_requested"], "zai,glm-5.3-flashx");
    assert_eq!(shown["usage"]["input_tokens"], 11);
    assert_eq!(shown["usage"]["cached_input_tokens"], 3);
    assert_eq!(shown["usage"]["output_tokens"], 7);
    assert_eq!(shown["usage"]["reasoning_output_tokens"], 2);
    assert_eq!(shown["usage"]["reported_turns"], 1);
    assert_eq!(shown["commands_completed"], 1);
    assert_eq!(shown["failed_commands"], 1);
    assert_eq!(shown["errors"], 1);
    assert_eq!(shown["turns_completed"], 1);
    let failures = shown["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 2);
    assert_eq!(failures[0]["cursor"], cursors[2]);
    assert_eq!(failures[0]["event"], "item.completed");
    assert_eq!(failures[0]["item_id"], "failed-command");
    assert_eq!(failures[0]["exit_code"], 2);
    assert_eq!(failures[1]["event"], "error");
    assert_eq!(
        failures[0]["summary"]["text"],
        "bell \u{7} and\nnewline".replace('\u{7}', "\u{FFFD}")
    );
    assert!(shown["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| warning
            == &Value::String("provider rate limited \u{7}".replace('\u{7}', "\u{FFFD}"))));
    assert_eq!(shown["final_report"]["text"], "αβγδεž漢🙂🙂🙂");
    assert_eq!(shown["final_report"]["truncated"], true);
}

#[cfg(unix)]
#[test]
fn non_utf8_missing_run_paths_are_safe_for_summary_events_and_listing() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let directory = TempDir::new().unwrap();
    let run = directory
        .path()
        .join("missing-parent")
        .join(OsString::from_vec(b"run-\xff".to_vec()));

    for report in [
        summarize(&run, 128).unwrap(),
        events(&run, 0, 10, None, 128).unwrap(),
    ] {
        assert_eq!(report["run_dir"], run.to_string_lossy().as_ref());
        assert_eq!(report["run_id"], "run-\u{fffd}");
    }
    assert_eq!(list_entry(&run).unwrap()["run_id"], "run-\u{fffd}");
}

#[test]
fn events_page_by_absolute_cursor_until_eof() {
    let directory = TempDir::new().unwrap();
    let run = directory.path().join("paged-run");
    let (starts, cursors) = event_offsets(
        &run.join("events.jsonl"),
        &[
            json!({"type": "thread.started", "thread_id": "thread-1"}),
            command_event("command-1", 0, "completed"),
            command_event("command-2", 0, "completed"),
            command_event("command-3", 0, "completed"),
        ],
    );

    let first = events(&run, 0, 2, None, 240).unwrap();
    assert_eq!(first["events"].as_array().unwrap().len(), 2);
    assert_eq!(first["events"][0]["cursor"], cursors[0]);
    assert_eq!(first["events"][1]["cursor"], cursors[1]);
    assert_eq!(first["next_cursor"], starts[2]);
    assert_eq!(first["has_more"], true);
    assert_eq!(first["trace"]["partial_line"], false);

    let second = events(&run, starts[2], 2, None, 240).unwrap();
    let rows = second["events"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["cursor"], cursors[2]);
    assert_eq!(rows[1]["cursor"], cursors[3]);
    assert_eq!(second["next_cursor"], cursors[3]);
    assert_eq!(second["has_more"], false);
}

#[test]
fn events_kind_filter_preserves_cursors() {
    let directory = TempDir::new().unwrap();
    let run = directory.path().join("filtered-run");
    let (_, cursors) = event_offsets(
        &run.join("events.jsonl"),
        &[
            json!({"type": "item.started", "item": {"type": "reasoning", "id": "reasoning"}}),
            command_event("command", 0, "completed"),
            json!({"type": "error", "error": "provider failed"}),
            json!({"type": "item.completed", "item": {
                "type": "file_change", "id": "edit", "changes": [{"kind": "add", "path": "src/lib.rs"}]
            }}),
        ],
    );

    let commands = events(&run, 0, 20, Some("command_execution"), 240).unwrap();
    let rows = commands["events"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["cursor"], cursors[1]);
    assert_eq!(rows[0]["item_type"], "command_execution");

    let errors = events(&run, 0, 20, Some("error"), 240).unwrap();
    let rows = errors["events"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["cursor"], cursors[2]);
    assert_eq!(rows[0]["event"], "error");
}

#[test]
fn partial_tail_defers_then_receives_appended_completion() {
    let directory = TempDir::new().unwrap();
    let run = directory.path().join("resumed-run");
    let trace = run.join("events.jsonl");
    let (_, cursors) = event_offsets(
        &trace,
        &[
            json!({"type": "thread.started"}),
            command_event("first", 0, "completed"),
        ],
    );
    let partial_start = cursors[1];
    let partial = serde_json::to_string(&json!({
        "type": "item.completed", "item": {"type": "command_execution", "id": "second"}
    }))
    .unwrap();
    let mut initial = fs::read_to_string(&trace).unwrap();
    initial.push_str(&partial);
    write(&trace, &initial);

    let before = events(&run, 0, 20, None, 240).unwrap();
    assert_eq!(before["events"].as_array().unwrap().len(), 2);
    assert_eq!(before["next_cursor"], partial_start);
    assert_eq!(before["has_more"], true);
    assert_eq!(before["trace"]["partial_line"], true);

    let completion = json!({"type": "item.completed", "item": {
        "type": "command_execution", "id": "second", "exit_code": 0
    }});
    let mut appended = initial;
    appended.push('\n');
    appended.push_str(&serde_json::to_string(&completion).unwrap());
    appended.push('\n');
    write(&trace, &appended);

    let resumed = events(&run, partial_start, 20, None, 240).unwrap();
    let rows = resumed["events"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["item_id"], "second");
    assert_eq!(rows[0]["cursor"], partial_start + partial.len() as u64 + 1);
    assert_eq!(rows[1]["item_id"], "second");
    assert_eq!(rows[1]["exit_code"], 0);
    assert_eq!(rows[1]["cursor"], fs::metadata(&trace).unwrap().len());
    assert_eq!(resumed["has_more"], false);
    assert_eq!(resumed["trace"]["partial_line"], false);
}

#[test]
fn detail_is_isolated_bounded_and_sanitized() {
    let directory = TempDir::new().unwrap();
    let run = directory.path().join("detail-run");
    let (_, cursors) = event_offsets(
        &run.join("events.jsonl"),
        &[
            json!({"type": "thread.started"}),
            json!({"type": "item.completed", "item": {
                "type": "command_execution",
                "id": "chosen",
                "command": "chosen command",
                "aggregated_output": "αβγδεž漢🙂🙂🙂🙂 tail \u{7}\n\tkept"
            }}),
            command_event("other", 0, "completed"),
        ],
    );

    let bounded = detail(&run, cursors[1], 10).unwrap();
    assert_eq!(bounded["schema_version"], 1);
    assert_eq!(bounded["cursor"], cursors[1]);
    assert_eq!(bounded["item_id"], "chosen");
    assert_eq!(bounded["detail"]["text"], "αβγδεž漢🙂🙂🙂");
    assert_eq!(bounded["detail"]["truncated"], true);
    assert!(bounded.get("events").is_none());
    assert!(bounded.get("recent").is_none());

    let sanitized = detail(&run, cursors[1], 240).unwrap();
    assert_eq!(
        sanitized["detail"]["text"],
        "αβγδεž漢🙂🙂🙂🙂 tail \u{7}\n\tkept".replace('\u{7}', "\u{FFFD}")
    );
    let missing = detail(&run, 1, 240).unwrap_err();
    assert!(missing
        .to_string()
        .contains("no complete event at that cursor"));
}

#[test]
fn malformed_receipt_and_trace_records_generate_warnings() {
    let directory = TempDir::new().unwrap();
    let run = directory.path().join("malformed-run");
    write(&run.join("status.json"), "{not json");
    let trace = run.join("events.jsonl");
    event_offsets(
        &trace,
        &[
            json!({"type": "worker.unknown", "message": "future event"}),
            json!({"type": "thread.started"}),
        ],
    );
    let existing = fs::read_to_string(&trace).unwrap();
    write(&trace, &format!("{existing}{{broken\n[1,2]\n"));

    let shown = summarize(&run, 240).unwrap();
    assert_eq!(shown["outcome"], "receipt_pending");
    assert_eq!(shown["needs_attention"], true);
    let warnings = shown["warnings"].as_array().unwrap();
    assert!(warnings
        .iter()
        .any(|warning| warning.as_str().unwrap().starts_with("receipt unreadable:")));
    assert!(warnings.iter().any(|warning| warning.as_str().unwrap()
        == "trace contains skipped records; counts may be incomplete"));

    let page = events(&run, 0, 20, None, 240).unwrap();
    assert_eq!(page["trace"]["malformed_lines"], 2);
    assert_eq!(page["trace"]["oversized_lines"], 0);
    let rows = page["events"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["event"], "worker.unknown");
    assert_eq!(rows[0]["item_type"], "");
    assert_eq!(rows[0]["summary"]["text"], "worker.unknown");
    assert_eq!(rows[1]["event"], "thread.started");
}

#[test]
fn failed_noncommand_tools_and_started_items_remain_visible() {
    let directory = TempDir::new().unwrap();
    let run = directory.path().join("pending");
    let (_, cursors) = event_offsets(
        &run.join("events.jsonl"),
        &[
            json!({"type": "item.started", "item": {"id": "patch", "type": "file_change", "status": "in_progress"}}),
            json!({"type": "item.completed", "item": {"id": "patch", "type": "file_change", "status": "failed", "changes": [{"kind": "update", "path": "file.rs"}]}}),
            json!({"type": "item.completed", "item": {"id": "tool", "type": "mcp_tool_call", "status": "failed", "error": {"message": "offline"}}}),
            json!({"type": "item.started", "item": {"id": "active", "type": "command_execution", "command": "sleep 1"}}),
        ],
    );

    let shown = summarize(&run, 240).unwrap();
    assert_eq!(shown["outcome"], "receipt_pending");
    assert_eq!(shown["failed_items"], 2);
    assert_eq!(shown["failed_commands"], 0);
    assert_eq!(shown["file_changes"], 1);
    assert_eq!(shown["needs_attention"], true);
    let active = shown["active_items"].as_array().unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0]["item_id"], "active");
    assert_eq!(active[0]["cursor"], cursors[3]);
    assert_eq!(shown["usage"]["reported_turns"], 0);
    let failures = shown["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 2);
    assert_eq!(failures[0]["item_id"], "patch");
    assert_eq!(failures[1]["item_id"], "tool");
}

#[test]
fn bounded_reports_omit_reasoning_and_default_tool_output() {
    let directory = TempDir::new().unwrap();
    let run = directory.path().join("bounded");
    let reasoning = "reasoning-content-not-for-summary";
    let output = "raw-output-not-for-summary".repeat(5000);
    let (_, cursors) = event_offsets(
        &run.join("events.jsonl"),
        &[
            json!({"type": "item.completed", "item": {"type": "reasoning", "text": reasoning}}),
            json!({"type": "item.completed", "item": {"id": "tool", "type": "command_execution", "command": "echo done", "aggregated_output": output, "exit_code": 0}}),
        ],
    );

    let entry = list_entry(&run).unwrap();
    assert!(!entry.to_string().contains(reasoning));
    assert!(!entry.to_string().contains("raw-output-not-for-summary"));
    let shown = summarize(&run, 240).unwrap();
    assert!(!shown.to_string().contains(reasoning));
    assert!(!shown.to_string().contains("raw-output-not-for-summary"));
    assert!(shown.to_string().len() < 5000);
    let page = events(&run, 0, 20, None, 240).unwrap();
    assert!(!page.to_string().contains(reasoning));
    assert!(!page.to_string().contains("raw-output-not-for-summary"));

    let reasoning_detail = detail(&run, cursors[0], 240).unwrap();
    assert_eq!(reasoning_detail["detail"]["text"], "[reasoning omitted]");
    let command_detail = detail(&run, cursors[1], 30).unwrap();
    assert_eq!(
        command_detail["detail"]["text"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        30
    );
    assert_eq!(command_detail["detail"]["truncated"], true);
}

#[test]
fn resumed_turns_reuse_ids_not_cursors_and_accumulate_usage() {
    let directory = TempDir::new().unwrap();
    let run = directory.path().join("resumed");
    let (_, cursors) = event_offsets(
        &run.join("events.jsonl"),
        &[
            json!({"type": "turn.started"}),
            command_event("item_1", 1, "failed"),
            json!({"type": "turn.completed", "usage": {"input_tokens": 10, "output_tokens": 2}}),
            json!({"type": "turn.started"}),
            command_event("item_1", 0, "completed"),
            json!({"type": "turn.completed", "usage": {"input_tokens": 20, "output_tokens": 3}}),
        ],
    );
    let first = detail(&run, cursors[1], 240).unwrap();
    let second = detail(&run, cursors[4], 240).unwrap();
    assert_eq!(first["exit_code"], 1);
    assert_eq!(second["exit_code"], 0);
    let shown = summarize(&run, 240).unwrap();
    assert_eq!(shown["usage"]["input_tokens"], 30);
    assert_eq!(shown["usage"]["output_tokens"], 5);
    assert_eq!(shown["usage"]["reported_turns"], 2);
    assert_eq!(shown["commands_completed"], 2);
    assert_eq!(shown["failed_commands"], 1);
}

#[test]
fn unknown_payloads_do_not_leak_through_events_or_detail() {
    let directory = TempDir::new().unwrap();
    let run = directory.path().join("future");
    let (_, cursors) = event_offsets(
        &run.join("events.jsonl"),
        &[
            json!({"type": "item.completed", "item": {"type": "future_type", "text": "hidden-future-payload", "aggregated_output": "hidden-future-payload"}}),
            json!({"type": "future.event", "nested": {"reasoning": "hidden-future-payload"}}),
        ],
    );
    let page = events(&run, 0, 20, None, 240).unwrap();
    assert!(!page.to_string().contains("hidden-future-payload"));
    for cursor in cursors {
        let item = detail(&run, cursor, 240).unwrap();
        assert!(!item.to_string().contains("hidden-future-payload"));
        assert_eq!(
            item["detail"]["text"],
            "[no supported detail; inspect original artifact locally]"
        );
    }
}

#[test]
fn receipts_reject_nonobject_and_oversized_documents() {
    let directory = TempDir::new().unwrap();
    let nonobject = directory.path().join("array.json");
    write(&nonobject, "[]");
    let error = read_receipt(&nonobject).unwrap_err();
    assert!(error
        .to_string()
        .contains("worker receipt must be a JSON object"));

    let oversized = directory.path().join("oversized.json");
    write(
        &oversized,
        &format!("{{\"x\":\"{}\"}}", "x".repeat(1024 * 1024)),
    );
    let error = read_receipt(&oversized).unwrap_err();
    assert!(error.to_string().contains("worker receipt exceeds 1 MiB"));
}

#[test]
fn missing_trace_after_nonzero_cursor_is_not_treated_as_empty() {
    let directory = TempDir::new().unwrap();
    let run = directory.path().join("missing");
    let error = events(&run, 7, 20, None, 240).unwrap_err();
    let error = error.downcast_ref::<std::io::Error>().unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn usage_saturates_and_counts_each_reported_turn() {
    let maximum = json!({
        "input_tokens": u64::MAX,
        "cached_input_tokens": u64::MAX,
        "output_tokens": u64::MAX,
        "reasoning_output_tokens": u64::MAX
    });
    let mut usage = Usage::default();
    usage.add(&maximum);
    usage.add(&maximum);
    assert_eq!(usage.input_tokens, u64::MAX);
    assert_eq!(usage.cached_input_tokens, u64::MAX);
    assert_eq!(usage.output_tokens, u64::MAX);
    assert_eq!(usage.reasoning_output_tokens, u64::MAX);
    assert_eq!(usage.reported_turns, 2);
}

#[test]
fn text_limits_characters_and_sanitizes_controls() {
    assert_eq!(text("αβγδεž漢🙂🙂🙂🙂 tail", 10), "αβγδεž漢🙂🙂🙂");
    assert_eq!(
        text("one\u{7}\ttwo\nthree\u{1b}", 20),
        "one\u{FFFD}\ttwo\nthree\u{FFFD}"
    );
}
