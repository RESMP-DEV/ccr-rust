# SSE decoder terminal flush (PR #86)

Date: 2026-11-26
Branch: `fix/sse-decoder-foundation` at `c9ea7b0`
Scope: decoder module split plus end-of-stream flush; the translated-stream
tail fix is stacked in PR #87.

## What changed

- `src/sse.rs` became the directory module `src/sse/{mod,decoder,tests}.rs`
  with unchanged public paths. `SseFrameDecoder::finish()` returns a final
  frame that ended with a line terminator but never received its blank-line
  separator, or was terminated by a bare CR.
- `src/router/streaming.rs` calls `finish()` on every end-of-stream branch:
  normal upstream EOF, idle timeout (before the timeout event is sent), and
  upstream read error (before the error item is forwarded). A flushed frame
  that carries a terminal marker after a transport error cannot mark the
  attempt successful (`transport_error` guard).
- A `message_stop` whose payload is not valid JSON no longer counts as a
  clean completion, so a cut stop frame cannot clear route backoff.
- Recovered multi-line payloads are serialized with `SseFrame::to_sse_string`,
  which prefixes every line with `data: `.
- Decoder resync: an empty push keeps `SwallowLf` pending, a trailing bare CR
  during resync stays in `SwallowLf`, and a trailing bare CR is excluded from a
  parsed field value.

## Verification

| Gate | Command | Result |
| --- | --- | --- |
| Format | `cargo fmt --all -- --check` | exit 0 |
| Lint | `cargo clippy --all-features --locked -- -D warnings` | exit 0 |
| Tests | `cargo test --all-features --locked` | exit 0, 945 passed, 0 failed, 1 ignored |
| Ratchet | `python3 scripts/check_sloc.py` | exit 0, 45142/45142 |

New regression tests: `resync_empty_push_keeps_swallow_lf_pending`,
`resync_preserves_bare_cr_terminated_nonempty_line`,
`parse_frame_excludes_trailing_bare_cr_from_field_value`,
`malformed_message_stop_does_not_clear_route_backoff`,
`recovered_multiline_anthropic_payload_uses_one_data_field_per_line`.

## Non-claims

- No live fleet window was observed after this change; the deployed binary
  predates it. The measurements in the PR #87 receipt are pre-deployment.
- The decoder split does not change the wire format, only end-of-stream
  recovery and frame accounting.
