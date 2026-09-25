use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use super::stream::{scan, ScanStats};

const RECENT_LIMIT: usize = 5;
const RECEIPT_LIMIT: u64 = 1024 * 1024;

pub(super) fn text(value: &str, limit: usize) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() && character != '\n' && character != '\t' {
                '�'
            } else {
                character
            }
        })
        .take(limit)
        .collect()
}

fn preview(value: &str, limit: usize) -> Value {
    json!({"text": text(value, limit), "truncated": value.chars().count() > limit})
}

fn display_value(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

pub(super) fn read_receipt(path: &Path) -> Result<Value> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(RECEIPT_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > RECEIPT_LIMIT {
        bail!("worker receipt exceeds 1 MiB");
    }
    let receipt: Value = serde_json::from_slice(&bytes).context("invalid worker receipt JSON")?;
    if !receipt.is_object() {
        bail!("worker receipt must be a JSON object");
    }
    Ok(receipt)
}

fn final_preview(path: &Path, limit: usize) -> Result<Option<Value>> {
    if limit == 0 {
        return Ok(None);
    }
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let bytes_total = file.metadata()?.len();
    let mut bytes = Vec::new();
    file.take((limit as u64 + 1) * 4).read_to_end(&mut bytes)?;
    let mut result = preview(&String::from_utf8_lossy(&bytes), limit);
    if bytes_total > bytes.len() as u64 {
        result["truncated"] = json!(true);
    }
    Ok(Some(result))
}

#[derive(Default, Serialize)]
struct Usage {
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    reported_turns: u64,
}

impl Usage {
    fn add(&mut self, usage: &Value) {
        for (name, counter) in [
            ("input_tokens", &mut self.input_tokens),
            ("cached_input_tokens", &mut self.cached_input_tokens),
            ("output_tokens", &mut self.output_tokens),
            ("reasoning_output_tokens", &mut self.reasoning_output_tokens),
        ] {
            *counter = counter.saturating_add(usage[name].as_u64().unwrap_or(0));
        }
        self.reported_turns += 1;
    }
}

fn retain_recent(queue: &mut VecDeque<Value>, value: Value) {
    if queue.len() == RECENT_LIMIT {
        queue.pop_front();
    }
    queue.push_back(value);
}

fn scan_optional(
    path: &Path,
    after: u64,
    visit: impl FnMut(u64, &Value) -> bool,
) -> Result<ScanStats> {
    match scan(path, after, visit) {
        Err(error)
            if after == 0
                && error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(ScanStats::default())
        }
        result => result,
    }
}

pub(super) fn list_entry(path: &Path) -> Result<Value> {
    let summary = summarize(path, 0)?;
    let mut entry = serde_json::Map::new();
    for key in [
        "run_id",
        "thread_id",
        "outcome",
        "task_verified",
        "needs_attention",
        "model_requested",
        "elapsed_seconds",
        "commands_completed",
        "failed_commands",
        "failed_items",
        "file_changes",
        "usage",
    ] {
        entry.insert(key.to_string(), summary[key].clone());
    }
    Ok(Value::Object(entry))
}

pub(super) fn summarize(path: &Path, max_chars: usize) -> Result<Value> {
    let mut warnings = Vec::new();
    let receipt_path = path.join("status.json");
    let receipt = match read_receipt(&receipt_path) {
        Ok(receipt) => Some(receipt),
        Err(error) if !receipt_path.exists() => {
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                None
            } else {
                return Err(error);
            }
        }
        Err(error) => {
            warnings.push(text(&format!("receipt unreadable: {error}"), 240));
            None
        }
    };
    let mut thread_id = None;
    let mut usage = Usage::default();
    let mut commands = 0u64;
    let mut failed_commands = 0u64;
    let mut failed_items = 0u64;
    let mut file_changes = 0u64;
    let mut errors = 0u64;
    let mut turns_completed = 0u64;
    let mut recent = VecDeque::new();
    let mut failures = VecDeque::new();
    let mut active = BTreeMap::new();
    let mut active_omitted = 0u64;
    let trace_path = path.join("events.jsonl");
    let stats = scan_optional(&trace_path, 0, |cursor, event| {
        let kind = event["type"].as_str().unwrap_or("");
        if kind == "thread.started" {
            thread_id = event["thread_id"].as_str().map(|value| text(value, 128));
        }
        if kind == "turn.started" {
            active.clear();
        }
        if kind == "turn.completed" {
            turns_completed += 1;
            if event["usage"].is_object() {
                usage.add(&event["usage"]);
            }
        }
        let item = &event["item"];
        let item_type = item["type"].as_str().unwrap_or("");
        let item_id = item["id"].as_str().unwrap_or("");
        if kind == "item.started" && item_type != "reasoning" {
            if active.len() < 100 {
                active.insert(text(item_id, 128), compact(cursor, event, 180));
            } else {
                active_omitted += 1;
            }
        }
        if kind == "item.completed" {
            active.remove(&text(item_id, 128));
            let failed = item["status"] == "failed"
                || item["exit_code"].as_i64().is_some_and(|code| code != 0)
                || item.get("error").is_some_and(|error| !error.is_null());
            if failed {
                failed_items += 1;
                retain_recent(&mut failures, compact(cursor, event, 240));
            }
            if item_type == "command_execution" {
                commands += 1;
                if failed {
                    failed_commands += 1;
                }
            }
            if item_type == "file_change" {
                file_changes += item["changes"]
                    .as_array()
                    .map_or(0, |changes| changes.len() as u64);
            }
            if item_type != "reasoning" {
                retain_recent(&mut recent, compact(cursor, event, 180));
            }
        }
        if matches!(kind, "error" | "turn.failed") {
            errors += 1;
            retain_recent(&mut failures, compact(cursor, event, 240));
        }
        true
    })?;
    if stats.malformed_lines > 0 || stats.oversized_lines > 0 {
        warnings.push("trace contains skipped records; counts may be incomplete".to_string());
    }
    if stats.partial_line {
        warnings.push("partial trailing record deferred; retry with the same cursor".to_string());
    }
    if !trace_path.exists() {
        warnings.push("events.jsonl is not available".to_string());
    }
    let receipt = receipt.unwrap_or(Value::Null);
    let outcome = receipt["outcome"].as_str().unwrap_or("receipt_pending");
    let final_report = match final_preview(&path.join("final.txt"), max_chars) {
        Ok(result) => result,
        Err(error) => {
            warnings.push(text(&format!("final report unreadable: {error}"), 240));
            None
        }
    };
    if let Some(error) = receipt.get("error") {
        warnings.push(text(&display_value(error), 240));
    }
    if let Some(receipt_errors) = receipt["errors"].as_array() {
        for error in receipt_errors.iter().take(RECENT_LIMIT) {
            warnings.push(text(&display_value(error), 240));
        }
    }
    let failed_commands =
        failed_commands.max(receipt["failed_tool_commands"].as_u64().unwrap_or(0));
    Ok(json!({
        "schema_version": 1, "run_id": path.file_name().unwrap_or_default().to_string_lossy(),
        "run_dir": path.to_string_lossy(), "thread_id": thread_id.or_else(|| receipt["thread_id"].as_str().map(|value| text(value, 128))),
        "outcome": text(outcome, 80), "task_verified": false,
        "needs_attention": outcome != "completed" || failed_commands > 0 || failed_items > 0 || errors > 0 || !warnings.is_empty(),
        "model_requested": receipt["model"].as_str().map(|value| text(value, 128)),
        "provider_verified": false, "elapsed_seconds": receipt["elapsed_seconds"].as_f64(),
        "exit_code": receipt["exit_code"].as_i64(), "read_only": receipt["read_only"].as_bool(),
        "network_allowed": receipt["network_allowed"].as_bool(),
        "retry_not_before": receipt["retry_not_before"].as_f64(),
        "usage": usage, "commands_completed": commands, "failed_commands": failed_commands,
        "failed_items": failed_items, "file_changes": file_changes, "errors": errors, "turns_completed": turns_completed,
        "active_items": active.values().take(RECENT_LIMIT).collect::<Vec<_>>(),
        "active_items_omitted": active.len().saturating_sub(RECENT_LIMIT) as u64 + active_omitted,
        "recent": recent, "failures": failures, "warnings": warnings,
        "trace": stats, "stderr_bytes": fs::metadata(path.join("stderr.txt")).map(|meta| meta.len()).unwrap_or(0),
        "final_report": final_report
    }))
}

fn compact(cursor: u64, event: &Value, max_chars: usize) -> Value {
    let kind = event["type"].as_str().unwrap_or("unknown");
    let item = &event["item"];
    let item_type = item["type"].as_str().unwrap_or("");
    let source = if item_type == "reasoning" {
        "[reasoning omitted]".to_string()
    } else if let Some(command) = item["command"]
        .as_str()
        .filter(|_| item_type == "command_execution")
    {
        command.to_string()
    } else if let Some(message) = item["text"]
        .as_str()
        .filter(|_| item_type == "agent_message")
    {
        message.to_string()
    } else if item_type == "file_change" {
        let changes = item["changes"].as_array();
        changes
            .into_iter()
            .flatten()
            .take(5)
            .map(|change| {
                format!(
                    "{} {}",
                    text(change["kind"].as_str().unwrap_or("change"), 40),
                    text(change["path"].as_str().unwrap_or("?"), max_chars)
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    } else if matches!(kind, "error" | "turn.failed") {
        display_value(
            event
                .get("error")
                .or_else(|| event.get("message"))
                .unwrap_or(&Value::Null),
        )
    } else if item_type == "mcp_tool_call" {
        format!(
            "{} {}",
            text(item["server"].as_str().unwrap_or(""), max_chars),
            text(item["tool"].as_str().unwrap_or(""), max_chars)
        )
    } else {
        if item_type == "web_search" {
            text(item["query"].as_str().unwrap_or(kind), max_chars)
        } else {
            kind.to_string()
        }
    };
    let change_count = item["changes"].as_array().map_or(0, Vec::len);
    let mut summary = preview(&source, max_chars);
    if change_count > 5 {
        summary["truncated"] = json!(true);
    }
    json!({"cursor": cursor, "event": text(kind, 80), "item_type": text(item_type, 80),
        "item_id": item["id"].as_str().map(|value| text(value, 128)),
        "status": item["status"].as_str().map(|value| text(value, 80)),
        "exit_code": item["exit_code"].as_i64(), "summary": summary,
        "change_count": change_count, "changes_omitted": change_count.saturating_sub(5),
        "output_bytes": item["aggregated_output"].as_str().map(str::len)})
}

pub(super) fn events(
    path: &Path,
    after: u64,
    limit: usize,
    kind: Option<&str>,
    max_chars: usize,
) -> Result<Value> {
    let mut events = Vec::new();
    let stats = scan_optional(&path.join("events.jsonl"), after, |cursor, event| {
        if kind.is_none_or(|kind| event["type"] == kind || event["item"]["type"] == kind) {
            events.push(compact(cursor, event, max_chars));
        }
        events.len() < limit
    })?;
    Ok(
        json!({"schema_version": 1, "run_id": path.file_name().unwrap_or_default().to_string_lossy(),
        "run_dir": path.to_string_lossy(), "events": events, "next_cursor": stats.next_cursor,
        "has_more": stats.next_cursor < stats.file_bytes, "trace": stats}),
    )
}

pub(super) fn detail(path: &Path, cursor: u64, max_chars: usize) -> Result<Value> {
    let mut found = None;
    scan(&path.join("events.jsonl"), 0, |offset, event| {
        if offset == cursor {
            let mut result = compact(offset, event, max_chars);
            let item = &event["item"];
            let content = match item["type"].as_str() {
                Some("reasoning") => "[reasoning omitted]".to_string(),
                Some("command_execution") => {
                    item["aggregated_output"].as_str().unwrap_or("").to_string()
                }
                Some("agent_message") => item["text"].as_str().unwrap_or("").to_string(),
                Some("file_change") => item["changes"].to_string(),
                Some("mcp_tool_call") => {
                    json!({"result": item["result"], "error": item["error"]}).to_string()
                }
                Some("web_search") => item["query"].to_string(),
                Some("todo_list") => item["items"].to_string(),
                None if event["type"] == "turn.completed" => event["usage"].to_string(),
                None if event["type"] == "error" || event["type"] == "turn.failed" => {
                    display_value(
                        event
                            .get("error")
                            .or_else(|| event.get("message"))
                            .unwrap_or(&Value::Null),
                    )
                }
                _ => "[no supported detail; inspect original artifact locally]".to_string(),
            };
            result["detail"] = preview(&content, max_chars);
            result["schema_version"] = json!(1);
            found = Some(result);
        }
        offset < cursor
    })?;
    found.context("no complete event at that cursor; use a cursor from workers events/show")
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod report_tests;
