# SSE decoding: root cause, differential harness, and terminal-path fix, 2026-11-26

The question was whether the recurring tool-call JSON truncation ("EOF while
parsing an object") can be parsed, attributed, and fixed rather than dodged.
Answer: the upstream wire is clean, the loss is inside the router, one
translation path drops the tail of the stream in a measured 22 of 36 requests,
and that path is now fixed.

Agent: Codex on `zai,glm-5.4` through CCR. Machine evidence under
`build/reports/sse-audit/`; this receipt is the consolidated record.

## What was measured before any fix

| Probe | Scope | Result |
| --- | --- | --- |
| Upstream framing capture | 9 reachable provider streams, byte-exact | every frame blank-line terminated, LF only, no CR, no multi-line `data:`, proper terminal signal, ends with newline |
| Decoder differential harness | 15 captures, 13 adversarial, 1 control | 0 real-capture divergences; 5 adversarial divergences, all 5 losing bytes; gate exits 1 |
| Decoder conformance audit | `src/sse.rs`, `src/router/streaming.rs`, `src/router/translate_response.rs` | 25 findings: 11 data loss, 12 protocol violation, 2 cosmetic |
| Translation audit | every argument-carrying path, 6 protocol pairs | 14 enumerated drop conditions; D1 reproduced byte-exact |
| Live rate, small payloads | 120 requests, 2 shapes, 2 routes | 0/120 truncation, but this shape pair does not include the Responses path |
| Long-payload sweep | 143 requests, 5 sizes, 2 concurrencies, 2 shapes | 16 parse failures, all `provider_length_limited`; 0 router-attributable; 0 open-block signatures |
| Responses-path sweep | 36 requests, 3 sizes, 2 read speeds, 2 routes | 22 parse failures with the output cap not reached, 7 clean, 7 with no tool call |

The framing capture exonerates the providers. The harness proves the decoder
loses bytes only on adversarial framing that the reachable providers do not
emit. So the live loss has to be in the translation and termination paths.

## The decisive measurement

The Responses-path sweep is the one that reproduces the corpus, because the
corpus is Codex sessions and Codex calls `/v1/responses`.

Categorising its 36 records honestly, rather than by the probe's own headline:

| Category | Count | Evidence |
| --- | ---: | --- |
| No tool call returned | 7 | `accumulated_chars` 0 and `delta_events` 0, not truncation |
| Parsed ok | 7 | every one had `usage_output_tokens` 32768 |
| Parse failure | 22 | every one had `usage_output_tokens` 0 |

The split is exact. When the router forwarded the upstream usage event the
arguments parsed; when the usage event was missing the arguments did not
parse. Zero usage cannot come from a completed upstream stream, so the router
was dropping the stream tail, usage event included, on the failing requests.
`done_matches_accumulated` was true on all 22, so the loss happened before the
Responses adapter, in the chat stage.

The pre-fix report's "28 of 35" figure is inflated by the 7 no-tool-call
requests; the truncating count is 22.

## Root cause

Two independent defects in `src/router/openai_compat.rs`, on the path an
Anthropic-protocol upstream takes to an OpenAI or Responses client:

1. `convert_anthropic_stream_response_to_openai` set `sent_done` when the
   upstream `message_stop` was translated or a `[DONE]` marker was decoded,
   then discarded every later frame. A final `input_json_delta` carrying the
   closing brace was thrown away while the router still emitted `data: [DONE]`.
   The reproduction shows upstream `{"blob":"alpha-42"}` reaching the client as
   `{"blob":"alpha-42"`: one closing container short, at depth 1, not cut inside
   a string, which is the corpus signature exactly.
2. The same function called `SseFrameDecoder::push` but never `finish`, so a
   final frame that was newline-terminated but not blank-line-terminated was
   dropped whole. The previously deployed end-of-stream flush was wired into
   `src/router/streaming.rs` only and never reached this consumer.

Three further defects amplified it: `content_block_stop` was emitted only when
a finish reason arrived, a missing finish reason was mapped to `end_turn`, and
a repeated tool `id` or `name` re-emitted `content_block_start` and made a
conforming client discard its accumulated `partial_json`.

## The fix

- `src/router/openai_compat.rs`: `sent_done` replaced by `terminal_seen`, so a
  late argument frame is forwarded and `data: [DONE]` is emitted exactly once at
  stream end; `decoder.finish()` called once at end of stream and its frame fed
  through the same translation path.
- `src/router/streaming.rs`: `decoder.finish()` now runs before the idle-timeout
  return and before an upstream read error propagates, so no end-of-stream
  branch skips the flush.
- `src/router/translate_response.rs`: every open block closes exactly once on
  every end-of-stream branch; a missing finish reason reports a null `stop_reason`
  instead of a fabricated `end_turn`; `content_block_start` is guarded per tool
  index; accumulated tool arguments are validated at terminal time, an empty
  accumulation gets `{}`, and invalid JSON raises an Anthropic `event: error`.
- `src/router/responses_api/incremental_stream.rs`: `function_call` arguments
  are validated at finalisation, so a truncated item emits `response.failed`
  with `invalid arguments for function call <name>: <parse error>` instead of a
  `response.completed` that claims success.
- `src/sse.rs`: a discarded-frame counter, a corrected resync that resumes
  exactly after the next blank line, a bare-CR final terminator accepted by
  `finish()`, and refusal to emit a truncated multi-line tail.
- Structure: `src/sse.rs` and `src/router/streaming.rs` became directory modules
  (`src/sse/{mod,decoder,tests}.rs`,
  `src/router/streaming/{mod,openai_translated,anthropic,tests}.rs`) with the
  public paths unchanged, so the SLOC ratchet passes without weakening a gate.

## Verification

| Gate | Command | Result |
| --- | --- | --- |
| Format | `cargo fmt --check` | exit 0 |
| Lint | `cargo clippy --all-features --locked -- -D warnings` | exit 0 |
| Tests | `cargo test --all-features --locked` | exit 0, 971 passed, 0 failed, 1 ignored |
| Ratchet | `python3 scripts/check_sloc.py` | exit 0, 46485/46485 at `3634d7f`; `main` at `cb5f0d2` carries the identical tree |
| Harness gate | `cargo run --release -- --gate` in `build/reports/sse-audit/harness` | exit 1 both before and after; 5 data-losing divergences before, 4 after, 0 on real captures both times |

## Merge state

- PR #86 `fix/sse-decoder-foundation` merged as `2601f59`.
- PR #87 `fix/sse-tail-loss` merged as `cb5f0d2`; its merge tree is byte-identical to the reviewed head `3634d7f`.

## Review round

- `openai_compat.rs` now records the transport error and sends it to the body
  channel after the flushed tail, so a network failure is no longer reported as
  a clean `data: [DONE]`; the terminal marker is suppressed on that path and
  `read_error_after_newline_terminated_tail_forwards_tail_then_fails` covers it.
- `streaming/openai_translated.rs` keeps `transport_error` and skips the
  success terminal events after an error item was forwarded.
- A `message_stop` whose payload is not valid JSON no longer counts as a clean
  completion (`stop_payload_valid`), so a cut stop frame cannot clear
  route backoff; `malformed_message_stop_does_not_clear_route_backoff` covers it.
- Recovered multi-line payloads are serialized through `SseFrame::to_sse_string`,
  which prefixes every line with `data: `;
  `recovered_multiline_anthropic_payload_uses_one_data_field_per_line` covers it.
- Decoder resync: an empty push keeps `SwallowLf` pending, a trailing bare CR
  during resync stays in `SwallowLf`, and a trailing bare CR is excluded from a
  parsed field value; three `src/sse/tests.rs` cases cover them.
- The Kilo duplicate-tool-index warning is stale: a tool block starts once per
  upstream index through `tool_index_map`, and
  `a_repeated_tool_identity_does_not_start_the_block_twice` passes.
- A tool block starts only once both `id` and `name` are known; a block that
  never completes its identity opens at the terminal flush with the
  placeholder, and the fragments are replayed in front of it.
- Anthropic block indices are assigned in block-start order through
  `tool_index_map`, so a buffered tool cannot leave a gap that makes the
  client's block list shorter than the index its later events use.
- Block closure moved to the terminal flush, so a late argument frame can no
  longer follow its own `content_block_stop`, and the `{}` fallback cannot
  precede a late fragment.
- A transport error closes the emitted blocks before the body error, and an
  idle timeout closes the blocks without a success-shaped `message_stop`.
- Tool arguments are validated as JSON objects, and a stream with invalid
  arguments no longer clears the route's 429 backoff.

Worker evidence: decoder hardening `20261126-162033-rnx9xdn6`, chat-stage fix
`20261126-163908-c1ec4oj4`, terminal path `20261126-163937-5n0fymzj`,
Responses-argument validation `20261126-171323-ebqe0y64`, module split
`20261126-171253-veda2rhi`. The parent re-ran every gate above independently;
the workers' sandbox could not bind localhost ports, which is why their full
suites reported permission failures that do not reproduce here.

## Deployment

- `cargo install --path . --force` from
  `.worktrees/sse-terminal-merge` exited 0; previous binary kept at
  `~/.cargo/bin/ccr-rust.bak-20261127`.
- `ccr-service-restart main --wait 5m` exited 0: pid 72570 to 75653, idle wait
  4.8 s, health true.
- The merged `main` tree replaced that binary on 2026-11-26 at 22:37 PST:
  `cargo install --path . --force --locked` from a clean worktree at `373da89`
  exited 0, installing a 46,096,384-byte `~/.cargo/bin/ccr-rust`.
- `ccr-service-restart main --wait 5m` exited 0: pid 75653 to 8788, idle wait
  38.0 s, health true. `auth` restarted from pid 25522 to 12900 and `worker`
  from 74057 to 20026 (idle wait 9.2 s), both health true.
- `ocr` (3458) stayed on pid 15718 and the pre-merge binary: it was serving a
  live Claude Code turn of roughly 90,000 message tokens, so the idle gate
  timed out twice at 2 minutes rather than interrupt the turn. Retry with
  `ccr-service-restart ocr --wait 5m` once that loop is idle.
- Smoke on the deployed binary: streaming `POST /v1/chat/completions` for
  `zai,glm-5.4` returned a complete chunk sequence ending in
  `finish_reason:"stop"`, a usage chunk, and `[DONE]`; curl exited 0.

## Post-fix measurement

Two probes, run against the deployed binary.

Large payloads, the same 36-request sweep as before, 1 sample per cell:

| Measure | Pre-fix | Post-fix |
| --- | ---: | ---: |
| No tool call returned | 7 | 10 |
| Parsed ok | 7 | 8 |
| Parse failure | 22 | 18 |
| Failure terminal | `response.completed`, zeroed usage, 22 of 22 | `response.failed`, `invalid_tool_arguments`, 18 of 18 |
| `response.completed` after a failure | 22 of 22 | 0 of 18 |

This sweep saturates the model's output budget on nearly every request, so its
counts cannot separate router loss from an output-limit cut. What it does prove
is the failure-mode change: a truncated tool call is no longer presented as a
successful response. The client now receives
`response.failed` with `error.code` `invalid_tool_arguments` and a message such
as `invalid arguments for function call record_blob: EOF while parsing a string
at line 1 column 33847`, and no `response.completed` follows.

Small payloads, 20 requests, `max_output_tokens` 8192, 10 per tier, a payload the
model honours so the output cap is never reached:

| Measure | Value |
| --- | ---: |
| Requests | 20 |
| Arguments that parsed | 20 |
| `response.completed` | 20 |
| Requests with real upstream usage | 20, output tokens 28 to 73 |
| Tiers | minimax 10, zai 10 |

That is the clean result. When the model is not truncated by its own output
budget, the Responses path now delivers complete arguments and the upstream
usage event on every request. Pre-fix, the discriminator was the opposite: usage
present meant the arguments parsed, usage absent meant they did not.

One reading of the large-payload sweep must be rejected. The comparison worker
concluded that "the tail loss therefore persists", because all 18 post-fix
failures have no usage block. That inference does not hold: the fix makes a
failed response carry no usage block by design, since `response.completed` is
suppressed and the usage event lives on it. Missing usage after a
`response.failed` is the intended shape, not evidence of a drop. The residual 18
are consistent with the model spending its 32768-token budget on a ~50,000
character argument string, which is the same output-limit class the long-payload
sweep measured on the other two shapes.

Decoder harness, re-pointed at the split module through a path dependency on the
fixed crate: 4 divergences, down from 5. `bare_cr_line_endings` is fixed. The
four that remain are `field_without_colon` (1 byte, an empty data line's
newline), `oversized_frame_split_before_boundary` (the deliberate 1 MiB
pending-frame cap), `stream_ends_mid_line` (the deliberate WHATWG discard of a
mid-line tail), and `bom_prefixed_data` (BOM handling not implemented). Real
captures remain at 0 divergences. The gate still exits 1 because it has no
allowlist for the three deliberate deviations, so it is a detector today rather
than a pass/fail CI gate.

## Non-claims

- The corpus records no SSE chunk boundary and no provider finish reason, so it
  cannot by itself name the frame that was lost. The attribution rests on the
  code path, the byte-exact reproduction, and the live usage split above.
- The 22 truncations were measured on two routes, one tool, and forced tool
  calls. Other providers, other models, and non-tool responses were not
  measured.
- The long-payload sweep's 16 failures are genuine output-token-limit cuts, not
  router loss. That probe set no output cap of its own, so its failure count is
  a model-budget artefact and must not be read as a router rate.
- The 0/120 live-rate figure covers the chat-completions and messages shapes
  only. It does not cover the Responses path and does not establish that the
  corpus path was fixed.
- No fleet-wide post-deploy window has been observed yet. The post-fix probe is
  a controlled sample, not a production rate.
- `event: error` is emitted before a terminal `message_stop` whose `stop_reason`
  is null, rather than suppressing `message_stop` entirely. Clients that stop
  reading on the error event will not see the closing events.
