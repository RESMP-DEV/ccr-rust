use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn isolated_command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ccr-rust"));
    command.env_clear().env("HOME", root);
    for variable in ["SystemRoot", "WINDIR", "PATH", "RUST_BACKTRACE"] {
        if let Some(value) = std::env::var_os(variable) {
            command.env(variable, value);
        }
    }
    command
}

fn run_json(root: &Path, arguments: &[&str]) -> Output {
    isolated_command(root)
        .args([
            "workers",
            "--runs-dir",
            root.to_str().expect("runs directory is valid Unicode"),
            "--json",
        ])
        .args(arguments)
        .output()
        .expect("run ccr-rust workers")
}

fn worker_json(root: &Path, arguments: &[&str]) -> Value {
    let output = run_json(root, arguments);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "worker command failed: {arguments:?}\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "JSON command polluted stderr for {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(&stdout).unwrap_or_else(|error| {
        panic!("stdout was not one JSON document for {arguments:?}: {error}; stdout: {stdout}")
    })
}

fn worker_error(root: &Path, arguments: &[&str], expected: &str, expected_code: i32) {
    let output = run_json(root, arguments);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "command unexpectedly succeeded: {arguments:?}; stdout: {stdout}"
    );
    assert!(stdout.trim().is_empty(), "error polluted stdout: {stdout}");
    assert!(
        stderr.contains(expected),
        "missing {expected:?} in stderr for {arguments:?}: {stderr}"
    );
    assert_eq!(
        output.status.code(),
        Some(expected_code),
        "unexpected exit for {arguments:?}: {stderr}"
    );
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn receipt(path: &Path, outcome: &str) {
    write(
        &path.join("status.json"),
        &json!({"outcome": outcome}).to_string(),
    );
}

fn event_offsets(trace: &Path, events: &[Value]) -> (Vec<u64>, Vec<u64>) {
    let mut offset = 0;
    let mut starts = Vec::new();
    let mut cursors = Vec::new();
    let mut contents = String::new();
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
fn json_lifecycle_covers_list_show_events_and_detail() {
    let directory = TempDir::new().unwrap();
    let root = directory.path();
    for name in ["2026-09-24-a", "2026-09-24-c"] {
        receipt(&root.join(name), "completed");
        write(
            &root.join(name).join("final.txt"),
            &format!("private final report for {name}"),
        );
    }
    let run = root.join("2026-09-24-b");
    write(
        &run.join("status.json"),
        &json!({"outcome": "failed", "model": "zai,glm-5.3-flashx"}).to_string(),
    );
    let (starts, cursors) = event_offsets(
        &run.join("events.jsonl"),
        &[
            json!({"type": "thread.started", "thread_id": "thread-1"}),
            command_event("command-1", 0, "completed"),
            command_event("command-2", 2, "failed"),
            json!({"type": "error", "error": "provider unavailable"}),
        ],
    );
    write(&run.join("final.txt"), "αβγδεž漢🙂🙂🙂🙂 tail");

    let listing = worker_json(root, &["list", "--limit", "2"]);
    assert_eq!(listing["schema_version"], 1);
    assert_eq!(listing["total_runs"], 3);
    assert_eq!(listing["omitted_runs"], 1);
    let runs = listing["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0]["run_id"], "2026-09-24-c");
    assert_eq!(runs[1]["run_id"], "2026-09-24-b");
    assert!(!listing.to_string().contains("private final report"));

    let shown = worker_json(root, &["show", "2026-09-24-b", "--max-chars", "10"]);
    assert_eq!(shown["schema_version"], 1);
    assert_eq!(shown["outcome"], "failed");
    assert_eq!(shown["needs_attention"], true);
    assert_eq!(shown["model_requested"], "zai,glm-5.3-flashx");
    assert_eq!(shown["failed_commands"], 1);
    assert_eq!(shown["errors"], 1);
    assert_eq!(shown["final_report"]["text"], "αβγδεž漢🙂🙂🙂");
    assert_eq!(shown["final_report"]["truncated"], true);

    let first = worker_json(root, &["events", "2026-09-24-b", "--limit", "2"]);
    assert_eq!(first["events"].as_array().unwrap().len(), 2);
    assert_eq!(first["events"][1]["cursor"], cursors[1]);
    assert_eq!(first["next_cursor"], starts[2]);
    assert_eq!(first["has_more"], true);
    let second = worker_json(
        root,
        &[
            "events",
            "2026-09-24-b",
            "--after",
            &starts[2].to_string(),
            "--kind",
            "error",
        ],
    );
    let rows = second["events"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["event"], "error");
    assert_eq!(rows[0]["cursor"], cursors[3]);
    assert_eq!(second["has_more"], false);

    let item = worker_json(
        root,
        &[
            "detail",
            "2026-09-24-b",
            &cursors[2].to_string(),
            "--max-chars",
            "10",
        ],
    );
    assert_eq!(item["schema_version"], 1);
    assert_eq!(item["item_id"], "command-2");
    assert_eq!(item["exit_code"], 2);
    assert_eq!(item["detail"]["text"], "output fro");
    assert_eq!(item["detail"]["truncated"], true);
    assert!(item.get("events").is_none());
}

#[test]
fn validated_flags_use_meaningful_exit_codes_and_stderr() {
    let directory = TempDir::new().unwrap();
    let root = directory.path();
    let empty = worker_json(root, &["list"]);
    assert_eq!(empty["runs"].as_array().unwrap().len(), 0);
    assert_eq!(empty["total_runs"], 0);
    worker_error(root, &["show", "latest"], "no worker runs found", 1);
    let run = root.join("present");
    receipt(&run, "completed");
    write(&run.join("events.jsonl"), "{\"type\":\"thread.started\"}\n");
    let bytes = fs::metadata(run.join("events.jsonl")).unwrap().len();

    worker_error(root, &["show", "missing"], "worker run not found", 1);
    worker_error(
        root,
        &["detail", "present", "1"],
        "no complete event at that cursor",
        1,
    );

    let invalid = [
        (&["list", "--limit", "0"][..], "must be between 1 and 100"),
        (&["list", "--limit", "101"][..], "must be between 1 and 100"),
        (
            &["show", "present", "--max-chars", "0"][..],
            "must be between 1 and 16000",
        ),
        (
            &["show", "present", "--max-chars", "16001"][..],
            "must be between 1 and 16000",
        ),
        (
            &["events", "present", "--limit", "0"][..],
            "must be between 1 and 100",
        ),
        (
            &["events", "present", "--limit", "101"][..],
            "must be between 1 and 100",
        ),
        (
            &["events", "present", "--max-chars", "0"][..],
            "must be between 1 and 16000",
        ),
        (
            &["events", "present", "--max-chars", "16001"][..],
            "must be between 1 and 16000",
        ),
        (
            &["events", "present", "--after", "not-a-number"][..],
            "invalid value",
        ),
    ];
    for (arguments, expected) in invalid {
        worker_error(root, arguments, expected, 2);
    }
    worker_error(
        root,
        &["events", "present", "--after", &(bytes + 1).to_string()],
        &format!(
            "cursor {} exceeds worker stream snapshot size {bytes}",
            bytes + 1
        ),
        1,
    );
}

#[test]
fn human_rendering_is_bounded_and_control_sanitized() {
    let directory = TempDir::new().unwrap();
    let root = directory.path();
    let run = root.join("human");
    write(
        &run.join("status.json"),
        &json!({
            "outcome": "failed",
            "error": "one\nFORGED_WARNING\tmore\u{1b}",
            "model": "model\nFORGED_MODEL"
        })
        .to_string(),
    );
    event_offsets(
        &run.join("events.jsonl"),
        &[json!({"type": "item.completed", "item": {
            "type": "command_execution", "id": "item\nFORGED_ID", "command": "true", "exit_code": 0
        }})],
    );

    let output = isolated_command(root)
        .args([
            "workers",
            "--runs-dir",
            root.to_str().unwrap(),
            "show",
            "human",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("ATTENTION"));
    assert!(stdout.contains("one FORGED_WARNING more\u{FFFD}"));
    assert!(!stdout.contains("\nFORGED_"));
    assert!(!stdout.contains('\u{1b}'));
    assert!(!stdout.contains('\t'));
}
