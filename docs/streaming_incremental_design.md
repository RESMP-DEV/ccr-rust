# Streaming Incremental Design

This document defines an incremental (non-buffering) streaming design for `contrib/ccr-rust/src/router.rs`.

## 1) Current Buffering Points In `src/router.rs`

### Stream conversion paths (current)

1. `convert_anthropic_stream_response_to_openai` (`contrib/ccr-rust/src/router.rs:1361`)
- Buffers full upstream body with `to_bytes(body, usize::MAX)` (`contrib/ccr-rust/src/router.rs:1363`).
- Parses SSE only after full payload is materialized (`parse_sse_frames(&payload)`, `contrib/ccr-rust/src/router.rs:1384`).
- Builds one large output string and returns `Body::from(output)` (`contrib/ccr-rust/src/router.rs:1435`).

2. `convert_openai_stream_response_to_responses` (`contrib/ccr-rust/src/router.rs:1837`)
- Buffers full upstream body with `to_bytes(body, usize::MAX)` (`contrib/ccr-rust/src/router.rs:1839`).
- Parses SSE only after full payload is materialized (`parse_sse_frames(&payload)`, `contrib/ccr-rust/src/router.rs:1899`).
- Builds one large output string and returns `Body::from(output)` (`contrib/ccr-rust/src/router.rs:2140`).

### Related non-stream buffering points

- `convert_anthropic_json_response_to_openai`: `to_bytes` at `contrib/ccr-rust/src/router.rs:1320` (expected for non-stream JSON transform).
- `convert_openai_json_response_to_responses`: `to_bytes` at `contrib/ccr-rust/src/router.rs:1792` (expected for non-stream JSON transform).
- `handle_responses` request body read: `to_bytes` at `contrib/ccr-rust/src/router.rs:2181` (request decode/parse path).

### Existing incremental stream path (already present)

- `stream_response_translated` (`contrib/ccr-rust/src/router.rs:2783`) already uses a bounded `tokio::sync::mpsc` + `Body::from_stream` (`contrib/ccr-rust/src/router.rs:2793`, `contrib/ccr-rust/src/router.rs:2987`), but SSE parsing is line-based per raw network chunk and does not keep a carry-over buffer for frame boundary splits.

## 2) Desired Non-Buffering Dataflow

### 2.1 Anthropic SSE -> OpenAI SSE

Target pipeline:

1. Read upstream body incrementally as bytes.
2. Feed bytes into an incremental SSE frame decoder with carry-over buffer:
- Handles arbitrary chunk boundaries.
- Emits complete `(event, data)` frames only when delimiter (`\n\n`) is complete.
3. For each frame:
- If `data == [DONE]`, forward `[DONE]` once.
- Else parse JSON data; if `type` missing, derive from SSE `event:` field.
- Transform via `AnthropicToOpenAiResponseTransformer`.
- Emit downstream `data: <openai_chunk_json>\n\n`.
4. On `message_stop`, ensure terminal `[DONE]` is emitted.
5. Return streaming body (`Body::from_stream`) backed by bounded channel.

No full-payload `to_bytes`; no full-output string assembly.

### 2.2 OpenAI SSE -> Responses SSE

Target pipeline:

1. Read upstream OpenAI SSE incrementally as bytes.
2. Decode SSE frames incrementally (same frame decoder).
3. Maintain stream state:
- `response_id`, `created_at`, `model`
- `message_text`, `reasoning_text`
- `tools[index]` accumulator for `id`, `name`, `arguments`, `added`
- latest `usage`
4. Emit events as soon as data is available:
- `response.created` once.
- `response.output_item.added` for assistant message/tool call introduction.
- `response.output_text.delta` for text deltas.
- `response.reasoning_text.delta` for reasoning deltas.
- Tool argument fragments appended incrementally; finalized in `response.output_item.done`.
5. On terminal chunk (`finish_reason`/`[DONE]`):
- Emit `response.output_item.done` per item.
- Emit `response.completed` with final `output` + `usage`.
6. Return streaming body via bounded channel.

No full-payload `to_bytes`; no full-output string assembly.

## 3) Error And Rate-Limit Behavior

### Current behavior summary

- Upstream `429` in protocol adapters is mapped to `TryRequestError::RateLimited` (`contrib/ccr-rust/src/router.rs:2535`, `contrib/ccr-rust/src/router.rs:2684`) and handled by tier fallback in `handle_messages` (`contrib/ccr-rust/src/router.rs:1096`).
- If all tiers fail (including repeated rate limits), router returns `503` JSON (`contrib/ccr-rust/src/router.rs:1150`).
- `/v1/responses` streaming conversion maps non-OK upstream response to `event: response.failed` SSE and forces HTTP `200` (`contrib/ccr-rust/src/router.rs:1854` to `contrib/ccr-rust/src/router.rs:1875`).
- Anthropic->OpenAI stream converter currently passes through non-OK status/body without SSE remapping (`contrib/ccr-rust/src/router.rs:1375` to `contrib/ccr-rust/src/router.rs:1377`).

### Target behavior

1. Non-streaming endpoints:
- Keep HTTP status semantics (`4xx/5xx` remain HTTP errors).
- Keep JSON error objects for parse/decode failures.
- For tier exhaustion (including rate-limit exhaustion), keep top-level `503`.

2. Streaming endpoints:
- Return protocol-native SSE error events, not mixed JSON bodies.
- `/v1/responses` stream failures: `response.failed` as terminal error event.
- OpenAI chat stream failures: emit OpenAI-compatible error frame, then `[DONE]`.
- Include `retry_after` in error payload when 429 metadata is available.

3. Mid-stream failures:
- Emit one terminal failure event when possible, then close stream.
- Do not emit `completed` after a terminal failure.

## 4) Backpressure Strategy (Bounded Channel + Drop/Abort)

Use bounded `tokio::sync::mpsc` with `SSE_BUFFER_SIZE` (`contrib/ccr-rust/docs/configuration.md:205`) for all stream conversion paths.

Policy:

1. Base behavior
- Apply backpressure with bounded queue (no unbounded buffering).
- Continue tracking `ccr_stream_backpressure_total` (`contrib/ccr-rust/docs/observability.md:21`).

2. Drop/coalesce behavior (delta-only)
- Coalesce high-frequency delta events when queue is full:
  - text deltas
  - reasoning deltas
  - tool argument fragments
- Never drop protocol-critical lifecycle events:
  - created/start
  - output item added/done
  - completed/failed
  - `[DONE]` or equivalent terminal marker

3. Abort behavior
- If queue remains blocked beyond timeout, abort upstream read task and terminate downstream stream.
- If client disconnects (`tx.closed()`), abort upstream immediately.
- On abort, emit terminal failure event if channel still writable; otherwise close.

## 5) Test Plan (Specific Scenarios)

Primary files:
- `contrib/ccr-rust/tests/integration_responses.rs`
- `contrib/ccr-rust/tests/integration_claude_code.rs`
- new focused unit tests near SSE decoder/state logic in `contrib/ccr-rust/src/router.rs`

### A. Chunk boundary scenarios

1. `anthropic_to_openai_stream_handles_split_sse_frame_boundaries`
- Split `event:`/`data:`/`\n\n` across random byte boundaries.
- Assert identical output event sequence to unsplit baseline.

2. `openai_to_responses_stream_handles_split_json_boundaries`
- Split inside JSON tokens and UTF-8 boundaries.
- Assert no malformed events and correct final `response.completed`.

3. `stream_parser_handles_multi_frame_single_chunk_and_single_frame_multi_chunk`
- Mixed framing density in both directions.

### B. Early-fail scenarios

4. `responses_stream_emits_response_failed_when_upstream_errors_before_first_delta`
- Upstream returns non-OK before any stream delta.
- Assert terminal `response.failed` and no `response.completed`.

5. `chat_stream_emits_error_then_done_on_midstream_transport_error`
- Upstream disconnect after partial output.
- Assert one terminal error marker and stream close.

6. `non_stream_returns_503_when_all_tiers_rate_limited`
- Simulate all tiers returning 429.
- Assert HTTP 503 JSON error body.

### C. Tool delta scenarios

7. Extend existing tool-merge coverage (`test_responses_stream_merges_tool_call_deltas_across_chunks`):
- Interleave two tool indices and ensure independent argument accumulation.
- Verify `output_item.added` appears once per tool and `output_item.done` has full merged arguments.

8. `responses_stream_handles_tool_id_or_name_arriving_late`
- First delta has only arguments; later delta adds `id`/`name`.
- Assert deterministic item identity in final output.

### D. Backpressure scenarios

9. `stream_backpressure_records_metric_when_channel_full`
- Force tiny buffer (`SSE_BUFFER_SIZE=1`) and slow consumer.
- Assert backpressure counter increments.

10. `stream_abort_on_persistent_backpressure`
- Keep consumer stalled beyond timeout.
- Assert upstream task aborts and stream terminates with failure event (if writable).
