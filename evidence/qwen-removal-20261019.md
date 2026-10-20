# Qwen monthly-exhaustion removal, 2026-10-19

## Decision and change

The user reported that Qwen was out of utilization for the month and directed
removal. Live router logs immediately before the change showed Qwen 429s with
reset-style `Retry-After` values around 2,642,629 seconds, while the local
tracker correctly capped actionable backoff at 60 seconds. This supports a
monthly utilization/reset boundary rather than a credential failure.

The reviewed shared route chain changed to:

1. `zai,glm-5.3-flash`
2. `minimax,MiniMax-M3.1-Flash-Preview`

Qwen remains a registered provider in the approval policy with its credential
reference intact, but no active route or generated consumer provider dispatches
it. Preflight therefore classifies it as `registered_not_routed`. Re-entry
requires a dated approval after the monthly window recovers.

## Source and deployment evidence

- Source/deployed policy modules have SHA-256
  `32a6a11fe867f47c4013f26105f2a2dc9b4740c9a95c96f8b2c9514680de0c78`;
  both `~/.local/share/ccr-routing/ccr_fallback_policy.py` and the
  `~/.claude-code-router/ccr_fallback_policy.py` link resolve to that body.
- Approved policy-body SHA-256:
  `c02255205d846f7839568099d1fc6e267f24d010c84593dea42966808bab5f67`.
- Pre-change snapshot: `20261019T191032-0700`, label
  `pre-qwen-removal`, 42 files, verify passed with no mismatch, missing, or
  unexpected files.
- `ccr-fallback-policy sync --json` atomically changed all four governed
  consumer configs. Resulting orders:
  - main/auth: `zai,glm-5.3` → `zai,glm-5.3-flash` →
    `minimax,MiniMax-M3.1-Flash-Preview`
  - worker: `zai,glm-5.3-flash` → `minimax,MiniMax-M3.1-Flash-Preview`
  - OCR: `openrouter,nvidia/nemotron-3-ultra-550b-a55b:free` →
    `zai,glm-5.3-flash` → `minimax,MiniMax-M3.1-Flash-Preview`
- A post-sync JSON inspection found no `qwen,*` tier and no active `qwen`
  provider in any generated consumer config.

## Qualification

- Proposed-policy preflight passed. Z.AI and MiniMax each authenticated on
  their model-list endpoint with HTTP 200. `deepseek`, `gmi`, and `qwen` were
  reported as `registered_not_routed`; none was probed.
- Focused CCR policy/recovery tests: 82 tests passed.
- `ruff check` and `ruff format --check` passed on the changed policy and
  policy-test files.
- Full offline dispatcher `python3 scripts/quality.py` passed: format, strict
  Clippy, all-features Rust tests, no-default-features check, Python helper
  tests, and vendored GP-routing checks. Receipt:
  `build/reports/quality/receipt.json`.

## Listener activation

- Auth listener (3461) passed two stable idle samples and restarted successfully.
- Worker listener (3457) passed two stable idle samples and restarted
  successfully.
- OCR (3458) remained busy (18 active requests when first gated; 16 at the
  latest check), so a detached 900-second idle-gated activation is running.
  Log: `~/.claude-code-router/logs/qwen-removal-activate-ocr.log`.
- Main (3456) remained active because this Codex turn itself was streaming
  through it, so a detached 900-second idle-gated activation is running rather
  than interrupting the live request. Log:
  `~/.claude-code-router/logs/qwen-removal-activate-main.log`.

## Diagnostic non-claims

- Qwen exhaustion explains wasted intermediate attempts, but it does not fully
  explain the client-visible terminal 429. At `2026-10-20T01:56:11.931Z`, a
  request emitted a Qwen pre-request token audit and then terminated before a
  `Trying qwen...` line. That sequence matches the process-wide retry-budget
  exhaustion branch, which exits the whole tier pass before later tiers are
  attempted while preserving the sweep's rate-limit classification. MiniMax
  succeeded 1.4 seconds later on another request. The retry-budget/fallback
  interaction remains a separate source issue after this route cordon.
- No claim is made about the exact Qwen monthly reset time; the large upstream
  hint was not converted into a calendar timestamp.
- The route cordon does not measure or change MiniMax account-level aggregate
  capacity across independently restarted listener processes.

## Deferred activation completion

- main (PID 18265, exited): {
  "idle": {
    "exit": 0,
    "ok": true,
    "output": "{\"ok\": true, \"url\": \"http://127.0.0.1:3456/metrics\", \"elapsed_seconds\": 801.519, \"samples\": 1586, \"active_requests\": 0.0, \"active_streams\": 0.0}",
    "port": 3456
  },
  "label": "com.kearm.ccr-rust",
  "listener": "main",
  "ok": true,
  "port": 3456,
  "stage": "restarted"
}
- ocr (PID 18266, exited): {
  "exit": 75,
  "label": "com.kearm.ccr-ocr",
  "listener": "ocr",
  "ok": false,
  "output": "{\"ok\": false, \"error\": \"listener stayed busy or metrics did not become stable before timeout\"}",
  "port": 3458,
  "stage": "idle"
}
- Recorded by deferred monitor at 2026-10-19T19:29:01.947224.
