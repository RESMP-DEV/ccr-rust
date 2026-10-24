# Tool-call efficiency receipt — 2026-10-18

## Scope

This work adds privacy-safe tool-call telemetry and improves timeout recovery for the deployed CCR worker launcher. It does not claim that any particular agent or model becomes more accurate; the telemetry makes repeated definitions, oversized arguments/results, and generated call rates measurable so later policy changes are evidence-based.

## Changes

- CCR now records per sanitized frontend/tool labels:
  - request-history tool calls and result sizes,
  - tool definition bytes sent on each model request,
  - non-streaming generated tool calls and argument bytes,
  - streamed tool-call assembly and argument bytes.
- `/v1/tool-metrics` returns bounded aggregate JSON. Prometheus exposes `ccr_tool_calls_total`, `ccr_tool_definition_bytes`, `ccr_tool_argument_bytes`, and `ccr_tool_result_bytes`. Redis persistence carries the aggregates.
- No raw tool schema, argument, result, model output, secret, or path other than the sanitized tool label is retained by the telemetry. The integration test checks known private payloads are absent.
- The live worker launcher at `~/.local/share/ccr-worker/worker.py` now writes a timeout `salvage` object containing a bounded resume command and session ID, partial report path/size, changed paths, command counts, failed commands, open checklist items, and bounded Git status/diff summaries. The live launcher is operational source outside this Git repository.

Live launcher hashes after repair:

- `worker.py`: `aa8d8a3cb353fa748981f7e72bc47280f6c5815ad6a214f8dd3ad4cc2c1a2f58`
- `test_worker.py`: `d04c5c1c4cf34a5f69842b7e25d26f98587d0a0bcd3bec84ca84fcead15996ee`

## Validation

CCR-Rust:

- `cargo fmt`
- `cargo clippy --all-targets --all-features -- -D warnings` — PASS
- `cargo test --all-features` — PASS
- `python3 scripts/quality.py` — PASS; dispatcher receipt: `build/reports/quality/receipt.json` (ignored runtime artifact). Checks covered format, strict Clippy, all-features tests, no-default-features, Python helpers, and vendored GP routing.

Deployed worker launcher:

- `ruff check worker.py test_worker.py` — PASS
- `ruff format --check worker.py test_worker.py` — PASS
- `uv run --no-project pytest -q test_worker.py` — PASS, 14 tests

## Non-claims

- No live model/provider request was used to validate telemetry; CCR coverage uses mocked upstreams and synthetic streaming frames.
- Tool telemetry does not detect duplicate calls by semantic content. It supplies per-tool counts and byte distributions for later duplicate analysis.
- Timeout salvage reports partial evidence only; it does not imply task success, valid code, or passing tests.
- `served_route` remains null because current Codex worker events expose no upstream provider/model identity. Requested route and fallback route list remain separate fields and must not be presented as served-route proof.
