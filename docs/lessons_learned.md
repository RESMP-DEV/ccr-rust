# Lessons Learned

Production incidents, debugging patterns, and architectural insights accumulated from operating CCR-Rust at scale (50–100+ concurrent agents across 7+ backend tiers).

---

## 1. Pseudo-SSE must handle ALL content block types (April 2026)

**Incident:** 100% of Kimi tool-calling tasks failed with Claude CLI exit code 1.

**Root cause:** `emit_anthropic_sse_events()` in `streaming.rs` only converted `Text` content blocks to SSE events. `ToolUse` and `Thinking` blocks were silently dropped via `_ => continue` in a match arm. When `forceNonStreaming: true` is enabled, ALL non-streaming responses pass through this function.

**Impact:** Every response with `stop_reason: tool_use` was delivered to Claude CLI missing the actual tool call blocks. Claude CLI saw "model wants to use tools" but found no tools in the content, generating `[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=tool_use`.

**Fix:** Handle all three `AnthropicContentBlock` variants with correct SSE event types:

| Block Type | `content_block_start`                                       | Delta Event(s)                       |
| ---------- | ----------------------------------------------------------- | ------------------------------------ |
| `Text`     | `{"type": "text", "text": ""}`                              | `text_delta`                         |
| `ToolUse`  | `{"type": "tool_use", "id": ..., "name": ..., "input": {}}` | `input_json_delta`                   |
| `Thinking` | `{"type": "thinking", "thinking": ""}`                      | `thinking_delta` + `signature_delta` |

**Lesson:** When converting between formats (JSON → SSE, Anthropic → OpenAI, etc.), always test with ALL content types the protocol supports, not just text. Catch-all `_ => continue` in match arms on enums is a code smell for protocol conversion code — it silently drops data instead of failing loudly.

**Detection pattern:** Compare captures at two levels:

1. HTTP-level captures (`~/.ccr-rust/captures/provider_*.json`) — shows what the backend actually returned
2. Task-level captures (`~/.ccr-rust/captures/agent_*.json`) — shows what Claude CLI received

If (1) has `tool_use` content but (2) shows no `assistant` event, the conversion layer is dropping blocks.

---

## 2. `forceNonStreaming` is a critical code path (April 2026)

**Context:** When running with `forceNonStreaming: true` (non-streaming responses are simpler to capture, debug, and replay), every response goes through `wrap_json_response_as_sse()` → `emit_anthropic_sse_events()`, which must be a perfect Anthropic Messages API SSE emitter.

**Lesson:** `forceNonStreaming` is not a "simplification" — it introduces a full protocol serialization layer. Any feature added to the streaming path must also work in the pseudo-SSE path. Test both paths whenever touching response handling.

**Checklist when modifying response types:**

- [ ] Does `emit_anthropic_sse_events()` handle the new block type?
- [ ] Does `stream_anthropic_response_with_tracking()` pass it through?
- [ ] Does `stream_response_translated()` (OpenAI → Anthropic) translate it?
- [ ] Are there unit tests for the new block type in all three paths?

---

## 3. Debug captures are essential for cross-layer debugging

**Pattern:** CCR-Rust sits between Claude CLI and the backend API. When something fails, the bug could be in:

1. The backend's response format
2. CCR-Rust's response transformation
3. CCR-Rust's SSE serialization
4. Claude CLI's parsing

**The `DebugCapture` system (`capture_success: true`)** saves both HTTP-level captures (what the backend returned) and is referenced by task-level captures (what the agent reported). Comparing these two layers immediately narrows the bug to the right component.

**Config:**

```json
{
  "DebugCapture": {
    "enabled": true,
    "capture_success": true,
    "output_dir": "~/.ccr-rust/captures",
    "max_body_size": 2097152
  }
}
```

**Analysis pattern:**

```bash
# HTTP-level: what did the backend return?
python3 -c "
import json
with open('captures/kimi_ccr-kimi_TIMESTAMP.json') as f:
    d = json.load(f)
resp = json.loads(d['response_body'])
print([c['type'] for c in resp['content']])
print('stop_reason:', resp['stop_reason'])
"

# Task-level: what did Claude CLI report?
python3 -c "
import json
with open('captures/agent_ccr-kimi_TIMESTAMP.json') as f:
    d = json.load(f)
events = json.loads(d['output'])
for e in events:
    print(e['type'], e.get('subtype', ''), e.get('errors', ''))
"
```

---

## 4. 429 rate limits cause cascade failures with `forceNonStreaming`

**Observation:** When the first Kimi request hits a 429, Claude CLI retries internally (via `api_retry` event). The retry may succeed but still produce an `error_during_execution` result if the conversion layer has bugs (like the tool_use block dropping above). This makes the 429 look like the root cause when it's actually an unrelated SSE conversion bug.

**Lesson:** Separate rate-limit failures from conversion failures in post-mortems. Check:

- Does the capture show a 200 response with valid content? → Conversion bug
- Does the capture show only 429s? → Actual rate limit issue

---

## 5. Match arm completeness for protocol enums

**Pattern:** Rust's exhaustive matching is a safety net, but `_ => continue` defeats it. When an enum gains a new variant (e.g., a new Anthropic content block type like `server_tool_use`), the catch-all silently handles it by doing nothing.

**Rule:** For protocol conversion code, prefer explicit arms for every variant. Use `_ =>` only with a `warn!()` log so new variants are visible:

```rust
// BAD: silent data loss
_ => continue,

// GOOD: explicit handling
AnthropicContentBlock::Text { .. } => { /* emit text */ }
AnthropicContentBlock::ToolUse { .. } => { /* emit tool_use */ }
AnthropicContentBlock::Thinking { .. } => { /* emit thinking */ }
// If Anthropic adds a new variant, this won't compile → forces update
```

---

## Appendix: Diagnostic Cheat Sheet

| Symptom                                                                      | Likely Cause                        | Check                                                           |
| ---------------------------------------------------------------------------- | ----------------------------------- | --------------------------------------------------------------- |
| `ede_diagnostic result_type=user last_content_type=n/a stop_reason=tool_use` | Pseudo-SSE dropping tool_use blocks | HTTP capture vs task capture comparison                         |
| `model not progressing for 300s`                                             | Backend timeout or stall            | Check ccr-rust logs + backend health                            |
| All tasks route to lowest tier                                               | Client caching model field          | Search your ccr-rust logs for `Direct routing`                  |
| 429 on all requests                                                          | Provider rate limit                 | `curl localhost:3456/metrics \| grep rate_limit`                |
| Exit code 1, events=[init, result] (no assistant)                            | Response conversion failure         | HTTP capture shows valid response but SSE conversion mangled it |
| Exit code 1, events=[init, api_retry×N, result]                              | API error after retries             | Check `error_status` in api_retry events                        |
