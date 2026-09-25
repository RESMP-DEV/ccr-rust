use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{scan, ScanStats, MAX_RECORD_BYTES};
use serde_json::Value;

fn write_file(path: &Path, contents: &[u8]) {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .unwrap();
    file.write_all(contents).unwrap();
    file.flush().unwrap();
}

fn append_file(path: &Path, contents: &[u8]) {
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(contents).unwrap();
    file.flush().unwrap();
}

fn temp_path(name: &str) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(name);
    (directory, path)
}

fn assert_no_bad_records(stats: &ScanStats) {
    assert_eq!(stats.malformed_lines, 0);
    assert_eq!(stats.oversized_lines, 0);
    assert!(!stats.partial_line);
}

#[test]
fn empty_file_preserves_cursor() {
    let (_directory, path) = temp_path("empty.jsonl");
    write_file(&path, b"");
    let stats = scan(&path, 0, |_offset, _value| true).unwrap();
    assert_eq!(stats.next_cursor, 0);
    assert_eq!(stats.file_bytes, 0);
    assert_eq!(stats.malformed_lines, 0);
    assert_eq!(stats.oversized_lines, 0);
    assert!(!stats.partial_line);
}

#[test]
fn scans_complete_objects_and_reports_nonobjects_as_malformed() {
    let (_directory, path) = temp_path("mixed.jsonl");
    write_file(
        &path,
        concat!(
            "{\"id\":1,\"kind\":\"unrecognized\",\"payload\":{\"nested\":[1]}}\n",
            "[]\n",
            "42\n",
            "\"text\"\n",
            "null\n",
            "{\"broken\"\n",
            "{\"id\":2}\n"
        )
        .as_bytes(),
    );
    let mut seen = Vec::new();
    let stats = scan(&path, 0, |offset, value: &Value| {
        seen.push((offset, value["id"].as_i64().unwrap()));
        true
    })
    .unwrap();

    let contents = fs::read(&path).unwrap();
    let first_end = contents.iter().position(|byte| *byte == b'\n').unwrap() as u64 + 1;
    assert_eq!(seen, vec![(first_end, 1), (contents.len() as u64, 2)]);
    assert_eq!(stats.malformed_lines, 5);
    assert_eq!(stats.oversized_lines, 0);
    assert!(!stats.partial_line);
}

#[test]
fn scans_from_a_valid_lf_cursor() {
    let (_directory, path) = temp_path("cursor.jsonl");
    let contents = b"{\"id\":1}\n{\"id\":2}\n{\"id\":3}\n";
    write_file(&path, contents);
    let first_end = contents.iter().position(|byte| *byte == b'\n').unwrap() as u64 + 1;
    let mut seen = Vec::new();
    let stats = scan(&path, first_end, |offset, value: &Value| {
        seen.push((offset, value["id"].as_i64().unwrap()));
        true
    })
    .unwrap();

    assert_eq!(seen, vec![(18, 2), (27, 3)]);
    assert_eq!(stats.next_cursor, contents.len() as u64);
    assert_no_bad_records(&stats);
}

#[test]
fn callback_can_stop_after_a_complete_record() {
    let (_directory, path) = temp_path("stop.jsonl");
    write_file(&path, b"{\"id\":1}\n{\"id\":2}\n{\"id\":3}\n");
    let mut seen = Vec::new();
    let stats = scan(&path, 0, |offset, value: &Value| {
        seen.push((offset, value["id"].as_i64().unwrap()));
        seen.len() < 2
    })
    .unwrap();

    assert_eq!(seen, vec![(9, 1), (18, 2)]);
    assert_eq!(stats.next_cursor, 18);
    assert!(!stats.partial_line);
}

#[test]
fn defers_partial_tail_and_resumes_after_completion() {
    let (directory, path) = temp_path("partial.jsonl");
    let first = b"{\"id\":1}\n";
    let second = b"{\"id\":2}\n";
    let partial = b"{\"id\":3";
    write_file(
        &path,
        [first.as_slice(), second.as_slice(), partial]
            .concat()
            .as_slice(),
    );
    let partial_start = (first.len() + second.len()) as u64;

    let mut seen = Vec::new();
    let stats = scan(&path, 0, |offset, value: &Value| {
        seen.push((offset, value["id"].as_i64().unwrap()));
        true
    })
    .unwrap();
    assert_eq!(seen, vec![(9, 1), (18, 2)]);
    assert_eq!(stats.file_bytes, partial_start + partial.len() as u64);
    assert!(stats.partial_line);

    append_file(&path, b"}\n{\"id\":4}\n");
    let mut retried = Vec::new();
    let stats = scan(&path, partial_start, |offset, value: &Value| {
        retried.push((offset, value["id"].as_i64().unwrap()));
        true
    })
    .unwrap();
    assert_eq!(retried, vec![(27, 3), (36, 4)]);
    assert_eq!(stats.next_cursor, 36);
    assert!(!stats.partial_line);
    assert!(directory.path().exists());
}

#[test]
fn skips_oversized_complete_record_but_parses_following_record() {
    let (_directory, path) = temp_path("oversized.jsonl");
    let payload_bytes = MAX_RECORD_BYTES + 1 - 8;
    let oversized = format!("{{\"d\":\"{}\"}}", "x".repeat(payload_bytes));
    write_file(
        &path,
        [oversized.as_bytes(), b"\n", b"{\"id\":2}\n"]
            .concat()
            .as_slice(),
    );
    let mut seen = Vec::new();
    let stats = scan(&path, 0, |offset, value: &Value| {
        seen.push((offset, value["id"].as_i64().unwrap()));
        true
    })
    .unwrap();

    let total_bytes = fs::metadata(&path).unwrap().len();
    assert_eq!(seen, vec![(total_bytes, 2)]);
    assert_eq!(stats.next_cursor, total_bytes);
    assert_eq!(stats.oversized_lines, 1);
    assert_eq!(stats.malformed_lines, 0);
    assert!(!stats.partial_line);
}

#[test]
fn accepts_record_at_exact_size_limit() {
    let (_directory, path) = temp_path("limit.jsonl");
    let payload_bytes = MAX_RECORD_BYTES - 8;
    let line = format!("{{\"d\":\"{}\"}}", "x".repeat(payload_bytes));
    write_file(&path, [line.as_bytes(), b"\n"].concat().as_slice());
    let mut count = 0;
    let stats = scan(&path, 0, |_offset, _value| {
        count += 1;
        true
    })
    .unwrap();
    assert_eq!(count, 1);
    assert_eq!(stats.next_cursor, MAX_RECORD_BYTES as u64 + 1);
    assert_eq!(stats.oversized_lines, 0);
}

#[test]
fn defers_oversized_partial_tail_without_counting_it() {
    let (_directory, path) = temp_path("oversized-partial.jsonl");
    let payload_bytes = MAX_RECORD_BYTES + 1 - 8;
    let partial = format!("{{\"d\":\"{}\"", "x".repeat(payload_bytes));
    write_file(&path, partial.as_bytes());
    let stats = scan(&path, 0, |_offset, _value| true).unwrap();

    assert_eq!(stats.next_cursor, 0);
    assert_eq!(stats.file_bytes, partial.len() as u64);
    assert!(stats.partial_line);
    assert_eq!(stats.oversized_lines, 0);
    assert_eq!(stats.malformed_lines, 0);

    append_file(&path, b"}\n");
    let stats = scan(&path, 0, |_offset, _value| true).unwrap();
    assert_eq!(stats.next_cursor, partial.len() as u64 + 2);
    assert!(!stats.partial_line);
    assert_eq!(stats.oversized_lines, 1);
}

#[test]
fn rejects_invalid_cursors() {
    let (_directory, path) = temp_path("cursors.jsonl");
    write_file(&path, b"{\"id\":1}\n{\"id\":2}");
    let file_bytes = fs::metadata(&path).unwrap().len();

    let error = scan(&path, file_bytes + 1, |_offset, _value| true)
        .unwrap_err()
        .to_string();
    assert!(error.contains("exceeds worker stream snapshot size"));

    let error = scan(&path, 5, |_offset, _value| true)
        .unwrap_err()
        .to_string();
    assert!(error.contains("not immediately after LF"));

    let error = scan(&path, file_bytes, |_offset, _value| true)
        .unwrap_err()
        .to_string();
    assert!(error.contains("not immediately after LF"));

    let stats = scan(&path, 9, |_offset, _value| true).unwrap();
    assert_eq!(stats.next_cursor, 9);
    assert_eq!(stats.file_bytes, file_bytes);
    assert!(stats.partial_line);
}

#[test]
fn reports_missing_file_as_io_error() {
    let (directory, path) = temp_path("missing.jsonl");
    let result = scan(&path, 0, |_offset, _value| true);
    assert!(result.is_err());
    assert!(directory.path().exists());
}

#[test]
fn scan_stats_serializes_public_shape() {
    let stats = ScanStats::default();
    let json = serde_json::to_value(&stats).unwrap();
    let object = json.as_object().unwrap();
    assert_eq!(object.len(), 5);
    assert!(object.contains_key("next_cursor"));
    assert!(object.contains_key("file_bytes"));
    assert!(object.contains_key("malformed_lines"));
    assert!(object.contains_key("oversized_lines"));
    assert!(object.contains_key("partial_line"));
}

#[test]
fn scan_snapshot_does_not_consume_appended_records() {
    let (_directory, path) = temp_path("snapshot.jsonl");
    let first = b"{\"id\":1}\n";
    write_file(&path, first);
    let mut seen = 0;
    let stats = scan(&path, 0, |_offset, _value| {
        seen += 1;
        append_file(&path, b"{\"id\":2}\n");
        true
    })
    .unwrap();
    assert_eq!(seen, 1);
    assert_eq!(stats.file_bytes, first.len() as u64);
    assert_eq!(stats.next_cursor, first.len() as u64);
    let stats = scan(&path, stats.next_cursor, |_offset, value| {
        assert_eq!(value["id"], 2);
        seen += 1;
        true
    })
    .unwrap();
    assert_eq!(seen, 2);
    assert_eq!(stats.next_cursor, (first.len() * 2) as u64);
}
