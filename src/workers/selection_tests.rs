use std::fs;
use std::path::Path;

use super::{report::list_entry, resolve_run, run_directories};
use serde_json::json;
use tempfile::TempDir;

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn receipt(path: &Path, outcome: &str) {
    write(
        &path.join("status.json"),
        &json!({
            "run_dir": path,
            "outcome": outcome,
            "task_verified": true,
            "model": "zai,glm-5.3-flashx"
        })
        .to_string(),
    );
}

#[test]
fn run_directories_recognize_artifacts_and_sort_new_runs_first() {
    let directory = TempDir::new().unwrap();
    let root = directory.path();
    for name in ["2026-09-24-a", "2026-09-24-c", "2026-09-24-b"] {
        let run = root.join(name);
        receipt(&run, "completed");
        write(&run.join("final.txt"), "private final report");
    }
    write(&root.join("ignored.txt"), "not a run");
    write(&root.join("2026-09-23-old").join("unused.txt"), "not a run");

    let paths = run_directories(root).unwrap();
    let names: Vec<_> = paths
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(names, ["2026-09-24-c", "2026-09-24-b", "2026-09-24-a"]);
    let entry = list_entry(&paths[0]).unwrap();
    assert_eq!(entry["run_id"], "2026-09-24-c");
    assert_eq!(entry["outcome"], "completed");
    assert_eq!(entry["task_verified"], false);
    assert_eq!(entry["model_requested"], "zai,glm-5.3-flashx");
    assert_eq!(entry["commands_completed"], 0);
    assert_eq!(entry["failed_commands"], 0);
    assert_eq!(entry["failed_items"], 0);
    assert_eq!(entry["file_changes"], 0);
    assert!(entry.get("recent").is_none());
    assert!(entry.get("final_report").is_none());
}

#[test]
fn empty_or_missing_root_has_no_runs() {
    let directory = TempDir::new().unwrap();
    let missing = directory.path().join("missing-root");
    assert!(run_directories(&missing).unwrap().is_empty());
    assert!(run_directories(directory.path()).unwrap().is_empty());
}

#[test]
fn latest_explicit_directory_trace_and_receipt_selectors_resolve() {
    let directory = TempDir::new().unwrap();
    let run = directory.path().join("finished");
    receipt(&run, "completed");
    write(&run.join("events.jsonl"), "{\"type\":\"thread.started\"}\n");
    let external = directory.path().join("task.status.json");
    let receipt_value = json!({"run_dir": run, "outcome": "completed", "task_verified": true});
    write(&external, &receipt_value.to_string());
    let expected = run.canonicalize().unwrap();

    let latest = resolve_run(directory.path(), "latest").unwrap();
    let explicit = resolve_run(directory.path(), "finished").unwrap();
    let directory_selector = resolve_run(directory.path(), run.to_str().unwrap()).unwrap();
    let receipt_selector =
        resolve_run(directory.path(), run.join("status.json").to_str().unwrap()).unwrap();
    let trace_selector =
        resolve_run(directory.path(), run.join("events.jsonl").to_str().unwrap()).unwrap();
    let external_selector = resolve_run(directory.path(), external.to_str().unwrap()).unwrap();
    for selected in [
        latest,
        explicit,
        directory_selector,
        receipt_selector,
        trace_selector,
        external_selector,
    ] {
        assert_eq!(selected, expected);
    }
}

#[test]
fn missing_and_unsupported_selectors_have_distinct_errors() {
    let directory = TempDir::new().unwrap();
    let root = directory.path();
    let run = root.join("finished");
    receipt(&run, "completed");
    write(&run.join("notes.txt"), "not selectable");

    let missing_root = resolve_run(&root.join("absent"), "latest").unwrap_err();
    assert!(missing_root.to_string().contains("no worker runs found"));
    let missing_run = resolve_run(root, "missing").unwrap_err();
    assert!(missing_run.to_string().contains("worker run not found"));
    let unsupported = resolve_run(root, run.join("notes.txt").to_str().unwrap()).unwrap_err();
    assert!(unsupported
        .to_string()
        .contains("expected a run directory, events.jsonl or JSON receipt"));
}

#[test]
fn completed_saved_receipt_does_not_claim_verified_success() {
    let directory = TempDir::new().unwrap();
    let run = directory.path().join("finished");
    write(
        &run.join("status.json"),
        &json!({"outcome": "completed", "task_verified": true, "retry_not_before": 1234})
            .to_string(),
    );
    write(&run.join("events.jsonl"), "{\"type\":\"turn.completed\"}\n");
    let shown = super::report::summarize(&run, 240).unwrap();
    assert_eq!(shown["task_verified"], false);
    assert_eq!(shown["provider_verified"], false);
    assert_eq!(shown["needs_attention"], false);
    assert_eq!(shown["retry_not_before"], 1234.0);
}
