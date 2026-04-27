// SPDX-License-Identifier: AGPL-3.0-or-later
//! Streaming turn capture state machine for tracking multi-thread responses.
//!
//! Inspired by the `TurnCaptureState` in codex-plugin-cc (codex.mjs lines 297-605).
//! Tracks threads, subagent collaboration, and completion inference when proxying
//! SSE streams.

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use tracing::{debug, info};

/// Tracks multi-thread streaming state during a proxied turn.
#[derive(Debug, Clone)]
pub struct TurnCaptureState {
    /// The root (main) thread ID that initiated the turn.
    pub root_thread_id: String,
    /// All known thread IDs (root + subagent threads).
    pub thread_ids: HashSet<String>,
    /// Maps each thread ID to its current turn ID.
    pub thread_turn_ids: HashMap<String, String>,
    /// Human-readable labels per thread (from thread name, agent nickname, etc.).
    pub thread_labels: HashMap<String, String>,
    /// The turn ID for the root thread (set once known).
    pub turn_id: Option<String>,
    /// In-flight subagent collaboration tool call IDs.
    pub pending_collaborations: HashSet<String>,
    /// Thread IDs with active subagent turns.
    pub active_subagent_turns: HashSet<String>,
    /// Whether a final_answer phase message has been seen on the root thread.
    pub final_answer_seen: bool,
    /// Whether the turn has been marked complete.
    pub completed: bool,
    /// The most recent agent message text from the root thread.
    pub last_agent_message: String,
    /// Accumulated reasoning summary sections.
    pub reasoning_summary: Vec<String>,
    /// Recorded file change items (type=fileChange, lifecycle=completed).
    pub file_changes: Vec<Value>,
    /// Recorded command execution items (type=commandExecution, lifecycle=completed).
    pub command_executions: Vec<Value>,
    /// Error from the stream, if any.
    pub error: Option<Value>,
}

impl TurnCaptureState {
    /// Create a new capture state for the given root thread.
    pub fn new(root_thread_id: String) -> Self {
        let mut thread_ids = HashSet::new();
        thread_ids.insert(root_thread_id.clone());
        Self {
            root_thread_id,
            thread_ids,
            thread_turn_ids: HashMap::new(),
            thread_labels: HashMap::new(),
            turn_id: None,
            pending_collaborations: HashSet::new(),
            active_subagent_turns: HashSet::new(),
            final_answer_seen: false,
            completed: false,
            last_agent_message: String::new(),
            reasoning_summary: Vec::new(),
            file_changes: Vec::new(),
            command_executions: Vec::new(),
            error: None,
        }
    }

    /// Route an SSE notification to update internal state.
    ///
    /// `method` is the notification type (e.g. `"thread/started"`, `"turn/completed"`).
    /// `params` is the JSON payload of the notification.
    pub fn apply_notification(&mut self, method: &str, params: &Value) {
        if self.completed {
            return;
        }
        debug!(method, root_thread = %self.root_thread_id, "processing notification");

        match method {
            "thread/started" => {
                if let Some(thread) = params.get("thread") {
                    let thread_id = json_str(thread, "id");
                    let label = thread_label_from_object(thread);
                    self.register_thread(&thread_id, label.as_deref());
                }
            }
            "thread/name/updated" => {
                let thread_id = json_str(params, "threadId");
                let name = params
                    .get("threadName")
                    .and_then(Value::as_str)
                    .map(String::from);
                self.register_thread(&thread_id, name.as_deref());
            }
            "turn/started" => {
                let thread_id = json_str(params, "threadId");
                self.register_thread(&thread_id, None);

                if let Some(turn) = params.get("turn") {
                    let turn_id = json_str(turn, "id");
                    self.thread_turn_ids
                        .insert(thread_id.clone(), turn_id.clone());

                    // If this is the root thread and we don't have a turn_id yet, set it.
                    if thread_id == self.root_thread_id && self.turn_id.is_none() {
                        self.turn_id = Some(turn_id);
                    }
                }

                // Non-root threads are subagent turns.
                if thread_id != self.root_thread_id {
                    self.active_subagent_turns.insert(thread_id);
                }
            }
            "item/started" => {
                let thread_id = params
                    .get("threadId")
                    .and_then(Value::as_str)
                    .map(String::from);
                if let Some(item) = params.get("item") {
                    self.record_item(item, Lifecycle::Started, thread_id.as_deref());
                }
            }
            "item/completed" => {
                let thread_id = params
                    .get("threadId")
                    .and_then(Value::as_str)
                    .map(String::from);
                if let Some(item) = params.get("item") {
                    self.record_item(item, Lifecycle::Completed, thread_id.as_deref());
                }
            }
            "error" => {
                if let Some(err) = params.get("error") {
                    self.error = Some(err.clone());
                }
            }
            "turn/completed" => {
                let thread_id = json_str(params, "threadId");

                if thread_id != self.root_thread_id {
                    self.active_subagent_turns.remove(&thread_id);
                    debug!(thread_id = %thread_id, "subagent turn completed");
                    return;
                }

                if let Some(turn) = params.get("turn") {
                    if self.turn_id.is_none() {
                        self.turn_id = Some(json_str(turn, "id"));
                    }
                }
                self.completed = true;
                info!(root_thread = %self.root_thread_id, "root turn completed");
            }
            _ => {}
        }
    }

    /// Returns `true` when the turn is complete — either explicitly via `turn/completed`
    /// on the root thread, or inferred (final answer seen, no pending collaborations,
    /// no active subagent turns).
    pub fn is_complete(&self) -> bool {
        if self.completed {
            return true;
        }
        // Inferred completion: final answer seen and all subagent work drained.
        self.final_answer_seen
            && self.pending_collaborations.is_empty()
            && self.active_subagent_turns.is_empty()
    }

    /// Check whether a notification from `thread_id` (and optional `turn_id`) belongs
    /// to this captured turn.
    pub fn belongs_to_turn(&self, thread_id: &str, turn_id: Option<&str>) -> bool {
        if !self.thread_ids.contains(thread_id) {
            return false;
        }
        let tracked_turn_id = self.thread_turn_ids.get(thread_id);
        match (tracked_turn_id, turn_id) {
            // If either side is unknown, accept the message.
            (None, _) | (_, None) => true,
            (Some(tracked), Some(incoming)) => tracked.as_str() == incoming,
        }
    }

    // ---- internal helpers ----

    fn register_thread(&mut self, thread_id: &str, label: Option<&str>) {
        self.thread_ids.insert(thread_id.to_owned());
        if let Some(l) = label {
            if !l.is_empty() {
                self.thread_labels
                    .insert(thread_id.to_owned(), l.to_owned());
            }
        }
    }

    fn record_item(&mut self, item: &Value, lifecycle: Lifecycle, thread_id: Option<&str>) {
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
        let is_root = thread_id.is_none_or(|tid| tid == self.root_thread_id);

        match item_type {
            "collabAgentToolCall" => {
                let id = json_str(item, "id");
                if is_root {
                    match lifecycle {
                        Lifecycle::Started => {
                            self.pending_collaborations.insert(id);
                        }
                        Lifecycle::Completed => {
                            self.pending_collaborations.remove(&id);
                        }
                    }
                }
                // Register any receiver threads.
                if let Some(receivers) = item.get("receiverThreadIds").and_then(Value::as_array) {
                    for r in receivers {
                        if let Some(tid) = r.as_str() {
                            self.register_thread(tid, None);
                        }
                    }
                }
            }
            "agentMessage" => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    if is_root && !text.is_empty() {
                        self.last_agent_message = text.to_owned();
                        if matches!(lifecycle, Lifecycle::Completed) {
                            let phase = item.get("phase").and_then(Value::as_str).unwrap_or("");
                            if phase == "final_answer" {
                                self.final_answer_seen = true;
                            }
                        }
                    }
                }
            }
            "reasoning" => {
                if matches!(lifecycle, Lifecycle::Completed) {
                    if let Some(summary) = item.get("summary").and_then(Value::as_str) {
                        if !summary.is_empty() {
                            self.reasoning_summary.push(summary.to_owned());
                        }
                    }
                }
            }
            "fileChange" => {
                if matches!(lifecycle, Lifecycle::Completed) {
                    self.file_changes.push(item.clone());
                }
            }
            "commandExecution" => {
                if matches!(lifecycle, Lifecycle::Completed) {
                    self.command_executions.push(item.clone());
                }
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    Started,
    Completed,
}

/// Extract a string field from a JSON object, returning an empty string if absent.
fn json_str(val: &Value, key: &str) -> String {
    val.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// Build a human-readable label from a thread JSON object.
fn thread_label_from_object(thread: &Value) -> Option<String> {
    // Prefer agentNickname, then name, then agentRole.
    for key in &["agentNickname", "name", "agentRole"] {
        if let Some(s) = thread.get(key).and_then(Value::as_str) {
            if !s.is_empty() {
                return Some(s.to_owned());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_new_state() {
        let state = TurnCaptureState::new("thread-1".into());
        assert_eq!(state.root_thread_id, "thread-1");
        assert!(state.thread_ids.contains("thread-1"));
        assert!(!state.completed);
        assert!(!state.final_answer_seen);
        assert!(!state.is_complete());
    }

    #[test]
    fn test_turn_started_sets_turn_id() {
        let mut state = TurnCaptureState::new("t-root".into());
        state.apply_notification(
            "turn/started",
            &json!({
                "threadId": "t-root",
                "turn": { "id": "turn-42" }
            }),
        );
        assert_eq!(state.turn_id, Some("turn-42".into()));
    }

    #[test]
    fn test_subagent_lifecycle() {
        let mut state = TurnCaptureState::new("t-root".into());

        // Start a subagent turn on a different thread.
        state.apply_notification(
            "turn/started",
            &json!({
                "threadId": "t-sub-1",
                "turn": { "id": "turn-sub" }
            }),
        );
        assert!(state.thread_ids.contains("t-sub-1"));
        assert!(state.active_subagent_turns.contains("t-sub-1"));

        // Complete the subagent turn.
        state.apply_notification(
            "turn/completed",
            &json!({
                "threadId": "t-sub-1",
                "turn": { "id": "turn-sub", "status": "completed" }
            }),
        );
        assert!(!state.active_subagent_turns.contains("t-sub-1"));
        assert!(!state.completed); // Root not completed yet.
    }

    #[test]
    fn test_inferred_completion() {
        let mut state = TurnCaptureState::new("t-root".into());

        // Simulate a final answer message on the root thread.
        state.apply_notification(
            "item/completed",
            &json!({
                "threadId": "t-root",
                "item": {
                    "type": "agentMessage",
                    "text": "Here is the answer.",
                    "phase": "final_answer"
                }
            }),
        );
        assert!(state.final_answer_seen);
        // No pending collaborations or subagent turns → inferred complete.
        assert!(state.is_complete());
    }

    #[test]
    fn test_inferred_blocked_by_pending_collab() {
        let mut state = TurnCaptureState::new("t-root".into());

        // Start a collaboration.
        state.apply_notification(
            "item/started",
            &json!({
                "threadId": "t-root",
                "item": {
                    "type": "collabAgentToolCall",
                    "id": "collab-1"
                }
            }),
        );
        // See final answer.
        state.apply_notification(
            "item/completed",
            &json!({
                "threadId": "t-root",
                "item": {
                    "type": "agentMessage",
                    "text": "Done.",
                    "phase": "final_answer"
                }
            }),
        );
        // Still not complete — collaboration pending.
        assert!(!state.is_complete());

        // Complete the collaboration.
        state.apply_notification(
            "item/completed",
            &json!({
                "threadId": "t-root",
                "item": {
                    "type": "collabAgentToolCall",
                    "id": "collab-1"
                }
            }),
        );
        assert!(state.is_complete());
    }

    #[test]
    fn test_explicit_completion() {
        let mut state = TurnCaptureState::new("t-root".into());
        state.apply_notification(
            "turn/completed",
            &json!({
                "threadId": "t-root",
                "turn": { "id": "turn-1", "status": "completed" }
            }),
        );
        assert!(state.completed);
        assert!(state.is_complete());
    }

    #[test]
    fn test_belongs_to_turn() {
        let mut state = TurnCaptureState::new("t-root".into());
        state
            .thread_turn_ids
            .insert("t-root".into(), "turn-1".into());

        assert!(state.belongs_to_turn("t-root", Some("turn-1")));
        assert!(!state.belongs_to_turn("t-root", Some("turn-other")));
        assert!(state.belongs_to_turn("t-root", None));
        assert!(!state.belongs_to_turn("t-unknown", Some("turn-1")));
    }

    #[test]
    fn test_file_changes_and_commands() {
        let mut state = TurnCaptureState::new("t-root".into());
        state.apply_notification(
            "item/completed",
            &json!({
                "threadId": "t-root",
                "item": { "type": "fileChange", "path": "src/main.rs" }
            }),
        );
        state.apply_notification(
            "item/completed",
            &json!({
                "threadId": "t-root",
                "item": { "type": "commandExecution", "command": "cargo test" }
            }),
        );
        assert_eq!(state.file_changes.len(), 1);
        assert_eq!(state.command_executions.len(), 1);
    }

    #[test]
    fn test_reasoning_summary() {
        let mut state = TurnCaptureState::new("t-root".into());
        state.apply_notification(
            "item/completed",
            &json!({
                "threadId": "t-root",
                "item": { "type": "reasoning", "summary": "Analyzed the codebase." }
            }),
        );
        assert_eq!(state.reasoning_summary, vec!["Analyzed the codebase."]);
    }

    #[test]
    fn test_thread_labels() {
        let mut state = TurnCaptureState::new("t-root".into());
        state.apply_notification(
            "thread/started",
            &json!({
                "thread": {
                    "id": "t-sub",
                    "name": "code-reviewer",
                    "agentNickname": "Rev"
                }
            }),
        );
        assert!(state.thread_ids.contains("t-sub"));
        // agentNickname takes priority.
        assert_eq!(state.thread_labels.get("t-sub"), Some(&"Rev".to_string()));
    }

    #[test]
    fn test_collab_registers_receiver_threads() {
        let mut state = TurnCaptureState::new("t-root".into());
        state.apply_notification(
            "item/started",
            &json!({
                "threadId": "t-root",
                "item": {
                    "type": "collabAgentToolCall",
                    "id": "collab-1",
                    "receiverThreadIds": ["t-recv-1", "t-recv-2"]
                }
            }),
        );
        assert!(state.thread_ids.contains("t-recv-1"));
        assert!(state.thread_ids.contains("t-recv-2"));
    }

    #[test]
    fn test_error_captured() {
        let mut state = TurnCaptureState::new("t-root".into());
        state.apply_notification(
            "error",
            &json!({
                "error": { "message": "something went wrong", "code": 500 }
            }),
        );
        assert!(state.error.is_some());
        assert_eq!(
            state.error.as_ref().unwrap()["message"],
            "something went wrong"
        );
    }

    #[test]
    fn test_no_ops_after_completion() {
        let mut state = TurnCaptureState::new("t-root".into());
        state.apply_notification(
            "turn/completed",
            &json!({ "threadId": "t-root", "turn": { "id": "t1" } }),
        );
        assert!(state.completed);

        // Further notifications are no-ops.
        state.apply_notification(
            "item/completed",
            &json!({
                "threadId": "t-root",
                "item": { "type": "fileChange", "path": "late.rs" }
            }),
        );
        assert!(state.file_changes.is_empty());
    }
}
