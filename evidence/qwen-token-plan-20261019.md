# Qwen Token Plan lane and timezone qualification — 2026-10-19

## Scope

Committed the already-live Qwen Token Plan machinery as a reviewed source
change: registered the plan-specific Anthropic provider, inserted
`qwen,qwen3.8-flash` as the worker chain's credit tier, updated the example
configuration, and replaced the obsolete request-quota Coding Plan guide with
a Token Plan operator runbook. The change does not alter protocol translation
or retry semantics and does not add a clock-driven automation.

## Live evidence

- The live provider used
  `https://token-plan.maas.qwencloudapi.com/apps/anthropic/v1` with an
  `sk-sp-` plan key referenced as `${CCR_QWEN_API_KEY}`. The final `/v1` was
  required: the suffix-less Anthropic path returned 404, while the `/v1`
  endpoint served a real completion on 2026-10-18.
- Both explicit CCR routes returned HTTP 200 with expected served models and
  usage fields: `qwen,qwen3.8-flash` served `qwen3.8-flash`, and
  `qwen,deepseek-v4.1-flash` served `deepseek-v4.1-flash`.
- The four coefficient passes ran through listener
  `http://127.0.0.1:3457/v1/chat/completions` on 2026-10-18/19:
  `qwen3.8-flash` night/day = 306/96 responses and 6,747,861/2,161,956
  processed tokens; `deepseek-v4.1-flash` night/day = 198/96 responses and
  4,451,027/2,158,668 processed tokens. Aggregate input:output ratios were
  3.9443:1, 4.0045:1, 3.9955:1, and 3.9969:1. Every response was HTTP 200,
  every served model matched, and no pass had a blocked event.
- Private raw evidence remains outside the repository in
  `~/.zcode/workspace/default/qwen-coefficient-logs/` (four JSONL logs plus
  `2026-10-18-pass-ledger.json`). The playbook receipt is §12.16, lines
  1230-1309, in `ai-value-playbook-2026-09-29.md`.

## Timezone proof

`zoneinfo` was used rather than hand arithmetic. On 2026-10-18/19:

- Qwen night start `2026-10-18T22:00+08:00` = `2026-10-18T14:00Z` =
  `2026-10-18T07:00-07:00`; night end `2026-10-19T08:00+08:00` =
  `2026-10-19T00:00Z` = `2026-10-18T17:00-07:00`.
- Monday-UTC DeepSeek peak starts `01:00Z` and `06:00Z`, which are the prior
  Pacific Sunday at `18:00-07:00` and `23:00-07:00`. The runbook therefore
  anchors weekday/off-peak decisions to UTC and calls out the date change and
  DST drift instead of storing a fixed PT hour.
- The night campaign's final response ended at `23:49:16.950Z`; day requests
  began at `00:00:02.091Z`, so no request spanned the Qwen day boundary.

## Validation

- `uv run --no-project python -m unittest discover -s scripts -p
  'test_ccr_fallback_policy.py' -v` — 29 tests, OK.
- `python3 scripts/quality.py` — PASS for format, clippy, rust-tests,
  no-default-features, python-helpers, and vendored-gp-routing; receipt:
  `build/reports/quality/receipt.json`.
- `jq empty config.example.json` — exit 0.
- `CCR_CLIENT_API_KEY=dummy DEEPSEEK_API_KEY=dummy MINIMAX_API_KEY=dummy
  OPENROUTER_API_KEY=dummy CCR_QWEN_API_KEY=dummy ccr-rust --config
  config.example.json validate` — valid; 5 providers, 10 Qwen models, and 4
  tiers including `qwen,qwen3.8-flash`.
- `git diff --check` — clean.

## Non-claims

- The Qwen console is login-gated. Credit deltas and therefore measured
  credits-per-1M coefficients were not available at commit time; the docs
  clearly mark the listed coefficients as pricing baselines, not console
  measurements.
- Token totals include `prompt_tokens_details.cached_tokens` as processed
  input. Cache-write billing treatment remains unverified.
- CCR deliberately does not change routes automatically at a wall-clock
  boundary. The runbook operationalizes explicit route selection; it does not
  claim a scheduler.
