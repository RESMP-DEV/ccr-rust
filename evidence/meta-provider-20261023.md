# Meta Model API (Muse) fallback tier — 2026-10-23

## Scope

Registered Meta's Model API (the Muse family) as a governed CCR provider and
added `meta,muse-spark-1.3` to the shared fallback chain as the last cloud
tier for the main interactive, auth, and worker lanes, per user direction,
on a key carrying free credits. This included pulling the public API
documentation into a repo runbook, storing the credential in the governed
store, live qualification through a scratch listener before any route
change, one router bug fix the qualification surfaced (`redacted_thinking`
content blocks), policy source + integrity pin + tests, deployment, and
idle-gated listener activation.

## Public contract evidence

- `https://dev.meta.ai/docs/overview` and `https://dev.meta.ai/docs/api-reference`:
  base URL `https://api.meta.ai/v1`; `Authorization: Bearer $MODEL_API_KEY`;
  three surfaces — OpenAI Responses (`POST /v1/responses`), OpenAI Chat
  Completions (`POST /v1/chat/completions`), and a native Anthropic-compatible
  Messages endpoint (`POST /v1/messages`, requires `max_tokens`); models
  `muse-wonder-1.0` ($4/$20 per Mtok), `muse-spark-1.3/1.2/1.1`
  ($1.25/$4.25 per Mtok, 1M context, 131072 output), contributor variants,
  `muse-image-1.0`, `muse-voice-transcribe-1.0`, `sam-3.1`.
- `https://dev.meta.ai/docs/authentication`: keys are `LLM_{id}_{secret}`;
  a malformed key (including one missing the `LLM_` prefix) returns
  `401 {"error":{"code":"invalid_api_key","type":"authentication_error"}}`.
- `https://dev.meta.ai/docs/error-handling`: Messages adapter rejects
  `stop_sequences`, `top_k`, `container`, `inference_geo` with 400; rate
  limits are per team (Standard 3,000 RPM / 4M TPM).
- `https://dev.meta.ai/docs/features/chat-completion`: Muse always reasons;
  `reasoning_effort: "none"` returns 400 on Chat Completions.

## Live qualification (before any route change)

- The user-supplied key string was missing the documented `LLM_` prefix;
  bare `{id}_{secret}` returned 401 `invalid_api_key` on `GET /v1/models`.
  With the prefix restored the key authenticates (HTTP 200) and lists the
  Muse catalog. The stored credential is the `LLM_`-prefixed form.
- `GET /v1/status` (unauthenticated): `{"is_alive":true,"service_status":"operational"}`.
- Direct `POST /v1/messages` (Anthropic surface, `anthropic-version`
  header accepted): 200, Anthropic `message` object, thinking billed as
  `output_tokens_details.thinking_tokens`, reasoning returned as
  `redacted_thinking` blocks. A 64-token cap produced 62 thinking tokens
  and no text — output caps must stay generous.
- Scratch listener (port 3472, `examples/meta.json`): first smoke run was
  3/4 — `chat/non-stream` returned the raw Anthropic envelope. Root cause:
  `AnthropicContentBlock` had no `redacted_thinking` variant, so
  `convert_anthropic_json_response_to_openai` failed its typed parse and
  passed the provider body through unchanged. Streaming and Responses paths
  translate raw JSON values, which is why they passed.
- Fix: `redacted_thinking` variant added to `AnthropicContentBlock`
  (`src/router/types.rs`); OpenAI-surface serialization drops it
  (`src/router/openai_compat.rs`); Anthropic SSE emission renders it
  faithfully as a start event with no deltas (`src/router/streaming.rs`);
  token estimation counts its payload (`src/router/dispatch.rs`). Regression
  test `redacted_thinking_block_converts_to_openai_shape` pins the shape.
- After the fix: smoke matrix 4/4 PASS (chat/responses × stream/non-stream)
  and a two-turn tool loop through the scratch listener — turn 1 returned a
  native `get_weather` tool call (`finish_reason: tool_calls`), turn 2
  replayed the tool call plus tool result and answered correctly with
  `cached_tokens: 113` (prompt caching active, no configuration).

## Governed policy change

- `scripts/ccr_fallback_policy.py`: `meta` shared provider (base
  `https://api.meta.ai/v1`, `${CCR_META_API_KEY}`, `protocol: anthropic`,
  `auth_header: authorization`, model `muse-spark-1.3`, maxtoken
  only-if-missing 32768) and `meta,muse-spark-1.3` inserted after
  `deepseek,deepseek-flash` and before `strata-b550`, keeping the local
  crawl-backs as the never-fails floor. Integrity pin
  `5a1270fe5626784d3555beffe64e01c79ac7e171b16fb628ad6b52e068b24205`.
- `scripts/test_ccr_fallback_policy.py`: chain tuples updated (three sites
  pin the exact chain), one `[-4]` positional assertion became `[-5]`, and
  `test_meta_muse_fallback_tier_is_pinned` pins position, provider fields,
  and the transformer. 43/43 policy tests pass.
- Credential: `CCR_META_API_KEY` added to
  `~/.claude-code-router/runtime-credentials.json` (mode 0600, atomic
  replace). The key never appears in a config, doc, log, or commit.

## Deployment

- Pre-change recovery snapshot `20261023T150935-0700` (verified clean).
- Source deployed to `~/.local/share/ccr-routing/ccr_fallback_policy.py`
  (backup `.bak-pre-meta-20261023`), live `fallback-policy.json` regenerated
  from the reviewed constants; `ccr-fallback-policy check --json` showed the
  expected four-consumer drift, `preflight --json` authenticated `meta`
  (HTTP 200 on `/models` with Authorization), `sync --json` rewrote all four
  consumer configs, and a post-sync check reported zero drift.
- Preflight pre-existing failures unchanged from the SenseNova receipt:
  `local27` connection refused (MLX backend not listening) and `strata-b550`
  connection reset. `holo4` (the concurrent session's new local tier)
  authenticated 200.

## Concurrent-session coordination

Two sessions edited the governed surface simultaneously; this receipt
records the interleaving so the state is auditable:

- A parallel session added a `holo4` direct-only route and, minutes later, a
  DeepSeek thinking-block transformer on the `feature/deepseek-thinking`
  worktree. My policy edits were layered additively after theirs deployed
  (Edit-tool staleness made every overlap fail loudly rather than clobber);
  their commit `a86db25` then reconciled the combined policy body to main.
- The shared installed binary was raced: my verified build
  (main checkout, with the `redacted_thinking` router fix) was replaced at
  16:07 by a `cargo install` from the unmerged
  `.worktrees/deepseek-thinking` branch, which lacks the router fix. That
  build served raw Anthropic envelopes to `/v1/chat/completions` clients on
  the meta tier (the string `redacted_thinking` exists in that binary via
  their transformer work, which initially masked the difference). I
  reinstalled from the main checkout (cargo install output confirms
  replacing the worktree build) and re-verified behavior before restarting
  listeners.
- Open item for the merge: `feature/deepseek-thinking` must merge against
  main's `redacted_thinking` router support and the reconciled policy, and
  the binary should then be reinstalled from the merged tree; until then a
  reinstall from that worktree regresses the meta chat-completions surface.

## Validation

- `python3 scripts/quality.py` — 12/12 checks PASS (receipt
  `build/reports/quality/receipt.json`), after re-pinning the SLOC ratchet
  (streaming.rs 1360→1373, total 40692→40746) and ruff-formatting the
  policy scripts.
- `python3 -m unittest discover -s scripts -p 'test_ccr_fallback_policy.py'`
  — 43 tests, OK.
- `cargo test --all-features --locked --lib` router suites — pass,
  including the new regression test.
- `jq empty examples/meta.json`; `ccr-rust --config examples/meta.json
  validate` — valid.
- `git diff --check` — clean.

## Live proof on governed listeners

- Workers (3457): explicit `meta,muse-spark-1.3` chat completion returned
  HTTP 200, `x-ccr-tier: meta`, content `META_WORKER_OK`,
  `finish_reason: stop`, model `muse-spark-1.3`. Streaming returned proper
  OpenAI chunks ending in a usage frame and `[DONE]`.
- Main (3456): the same explicit-route proof after its idle-gated restart
  returned HTTP 200, `x-ccr-tier: meta`, content `META_MAIN_OK`,
  `finish_reason: stop`, model `muse-spark-1.3` (250 total tokens). The
  first main proof attempt hit the pre-restart process 5 seconds before its
  activation completed and reproduced the raw-envelope leak, which is what
  exposed the binary-install race documented above.
- The main listener log also shows real traffic cascading onto the meta tier
  during the Z.AI rate-limit window before the proofs ran.

## Non-claims

- The free-credit balance was not quantified; only that the key
  authenticates and completes real requests. Spend monitoring is unmeasured.
- Reasoning does not carry across turns through this tier: CCR does not
  surface Muse reasoning items to Responses-surface clients, so multi-turn
  reasoning replay is unavailable. Single-turn quality and the tool loop are
  proven; long agentic sessions on this tier are not.
- `muse-wonder-1.0`, image, voice, and SAM models are unregistered and
  unprobed. Contributor-mode models are unregistered.
- Quota-exhaustion behavior (402/429 shapes, Retry-After) on this key was
  not exercised.
- The OCR/auth listeners carry the tier via the shared chain (on-disk
  configs verified) but only workers and main received explicit-route live
  proofs.
