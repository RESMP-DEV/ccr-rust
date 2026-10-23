# Qwen Codex fallback repair and MiniMax launch check — 2026-10-19

## Diagnosis

The main/worker Qwen `400 InvalidParameter` failures were reproduced with a
bounded Codex round trip and Qwen-only debug capture. Capture was enabled only
after an idle-gated restart, and both affected listeners were returned to
capture-disabled configuration with another idle-gated restart.

The captured upstream request was Anthropic-shaped, carried Codex tool
definitions, had `output_config: {"effort":"max"}`, and had no `max_tokens`.
A four-cell mutation matrix against the same captured body isolated the root
cause:

| Mutation | Result |
|---|---|
| Captured shape, no `max_tokens` | HTTP 400 `InvalidParameter` |
| Add `max_tokens: 16` only | HTTP 200, served `qwen3.8-flash` |
| Remove `output_config` only | HTTP 400 `InvalidParameter` |
| Add `max_tokens` and remove `output_config` | HTTP 200, served `qwen3.8-flash` |

The same tool-bearing request with `max_tokens: 32768` also returned HTTP 200,
served `qwen3.8-flash`, and ended normally. Qwen enforces Anthropic's required
output limit; `output_config` is not the trigger. The private capture remains
local at `~/.ccr-rust/captures/` and is not copied into this repository.

## Mitigation and routing

- Extended `maxtoken` with `only_if_missing: true`, preserving an explicit
  client limit while injecting a value when Codex omits it.
- Configured the Qwen provider with `max_tokens: 32768`, applied only when the
  field is absent.
- Kept the worker volume chain as GLM Flash → Qwen Flash → MiniMax → Qwen
  DeepSeek.
- Added `qwen,qwen3.8-max` immediately after the GLM-5.3 primary for main and
  auth, before GLM Flash and Qwen Flash.
- Made Qwen `deepseek-v4.1-flash` the final general fallback after MiniMax.

## MiniMax live launch check

At 2026-10-19T12:27Z, the Anthropic-compatible MiniMax `/models` endpoint
returned HTTP 200 and listed `MiniMax-M3.1-Flash` without a preview entry.
Direct message probes returned:

- requested `MiniMax-M3.1-Flash` → HTTP 200, response model
  `MiniMax-M3.1-Flash`;
- requested `MiniMax-M3.1-Flash-Preview` → HTTP 200, response model
  `MiniMax-M3.1-Flash-Preview`.

Both IDs are therefore accepted and self-report distinctly, but this small
exact-answer probe does not compare quality. CCR now canonicalizes M3-family
and legacy preview routes to the official non-preview ID before dispatch, so
old preview pins follow the launch automatically.

## Validation

- `uv run --no-project python -m unittest discover -s scripts -p
  'test_ccr_fallback_policy.py' -v` — 30 tests, OK.
- `cargo test transform::maxtoken --all-features --locked` — 10 tests, OK.
- `cargo test router::dispatch::tests::minimax --all-features --locked` —
  3 tests, OK.
- `cargo test transform::registry::tests::registry_builds_qwen
  --all-features --locked` — OK.
- `python3 scripts/quality.py` — PASS for format, clippy, rust-tests,
  no-default-features, python-helpers, and vendored-gp-routing; receipt:
  `build/reports/quality/receipt.json`.
- Dummy-credential `ccr-rust --config config.example.json validate` — valid.

## Live activation and acceptance

- Deployed policy SHA-256: `d84e6cbab5f06bfb6336313eaa8a03850cde5bf93b4b7e7bf5b36dd9ee7ee879`.
  `ccr-fallback-policy check --json` reports no drift, and preflight
  authenticated Z.AI, Qwen, and MiniMax.
- `ccr-fallback-policy sync --json` updated all four consumer configs. The
  first idle-gated activation wave loaded the new chains into main, worker,
  and auth.
- Live repaired Codex tool request: `codex-ccr --model qwen,qwen3.8-flash`
  returned `QWEN_REPAIRED`; worker metrics recorded one Qwen success and the
  router log shows `Success on qwen after 2.69s`.
- Legacy MiniMax redirect: a request for
  `minimax,MiniMax-M3.1-Flash-Preview` returned HTTP 200 with response model
  `MiniMax-M3.1-Flash` and `MINIMAX_REDIRECT_OK`.
- Qwen Max main-route probe returned HTTP 200, served `qwen3.8-max`, and
  answered `QWEN_MAX_OK`.
- Qwen DeepSeek final-fallback Codex tool request returned
  `QWEN_DEEPSEEK_OK` through `qwen,deepseek-v4.1-flash`.
- The first clean-binary install exposed that JSON `/health` and startup-safe
  idle gauges still live in the primary checkout's separate uncommitted
  recovery work. A temporary combined binary was built from this repair plus
  those four existing runtime files, preserving both changes; no unrelated
  recovery work was overwritten or committed. Final listener-state receipts
  are recorded below after every active workload drained.

## Non-claims

- The MiniMax probe proves ID availability and served-model identity, not
  relative model quality or throughput.
- No claim is made that Qwen DeepSeek's tool-loop behavior is equivalent to
  Qwen Flash; the max-token repair applies provider-wide, but DeepSeek receives
  its first broad operational exposure as the final fallback.
- Console credit coefficients remain pending.
