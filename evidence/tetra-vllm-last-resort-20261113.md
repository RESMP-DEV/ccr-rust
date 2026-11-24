# Tetra vLLM last-resort integration, 2026-11-13

Agent: Codex. This receipt records a source-prepared route integration and an
isolated live qualification. No production CCR listener, policy JSON, credential
file, or LaunchAgent was changed.

## Decision

The good integration shape is the existing Strata B550 pattern, not direct
ZeroTier routing:

1. A KeepAlive LaunchAgent owns a foreground SSH forward from
   `127.0.0.1:18317` to Tetra loopback `127.0.0.1:8317`.
2. CCR defines an OpenAI-protocol `tetra-vllm` provider at
   `http://127.0.0.1:18317/v1`.
3. The provider is single-flight, forces `reasoning_effort: none`, and injects
   a missing 32768-token output limit.
4. The route is final, after `local27`, because Tetra is much stronger than the
   local crawl-backs but is guest-owned and can disappear independently.

The source policy and tests now pin this order:

```text
minimax,MiniMax-M3.1-Flash
zai,glm-5.3-flash
deepseek,deepseek-flash
meta,muse-spark-1.3
strata-b550,qwen3.8-flash-next-coder
local27,qwen3.8-27b-mxfp4
tetra-vllm,deepseek-ai/DeepSeek-V4.1-Flash
```

The harmless bearer credential reference exists because shared providers require
an environment-backed credential. The remote vLLM endpoint itself accepts the
local loopback path without authentication; the SSH forward is the access
boundary.

## Isolated live qualification

A temporary SSH forward and scratch CCR listener on `127.0.0.1:13456` used this
provider shape:

```json
{
  "name": "tetra-vllm",
  "api_base_url": "http://127.0.0.1:18317/v1",
  "api_key": "${CCR_TETRA_VLLM_API_KEY}",
  "protocol": "openai",
  "models": ["deepseek-ai/DeepSeek-V4.1-Flash"],
  "maxInflight": 1,
  "force_reasoning_effort": "none",
  "transformer": {
    "use": [["maxtoken", {"max_tokens": 4096, "only_if_missing": true}]]
  }
}
```

Results through the scratch listener:

- `ccr-rust --config /tmp/ccr-tetra-audit/config.json validate`: exit 0.
- `/v1/models`: exposed both the explicit provider route and the derived bare
  model ID.
- Chat Completions returned `CCR_TETRA_OK` in 0.530 seconds, finish reason
  `stop`, and 0 reasoning tokens.
- Anthropic Messages returned `ANTHROPIC_TETRA_OK` in 0.852 seconds, finish
  reason `end_turn`, and 0 reasoning tokens.
- Responses returned `RESPONSES_TETRA_OK` in 0.362 seconds with status
  `completed` and 0 reasoning tokens.
- Chat SSE ended with a terminal usage chunk and `[DONE]` in 0.471 seconds.
- Responses SSE ended with `response.completed` in 0.294 seconds.
- A forced `get_weather` tool call returned valid arguments
  `{"city":"Seattle"}`; replaying that call and a synthetic tool result produced
  a normal final answer in 0.461 seconds. Both turns had 0 reasoning tokens.
- An isolated policy preflight against only this provider returned HTTP 200 on
  `/v1/models` and `ok: true`, policy SHA-256
  `bc218d4b0121a18525abf21612bc779a709a78f19e4cfecc7f3ca58f3f9a9b40`.

The scratch listener and temporary SSH forward were removed after validation.
No request was sent through a production CCR listener.

## Source and validation

Files added or changed by this candidate:

- `scripts/ccr_fallback_policy.py`
- `scripts/test_ccr_fallback_policy.py`
- `scripts/tetra_vllm_tunnel.sh`
- `deploy/com.kearm.tetra-vllm-tunnel.plist`
- `docs/local-operation.md`
- `docs/shared-fallback-policy.md`
- `CHANGELOG.md`
- this receipt

Validation:

- `plutil -lint deploy/com.kearm.tetra-vllm-tunnel.plist`: OK.
- `shellcheck scripts/tetra_vllm_tunnel.sh`: exit 0.
- `shfmt -i 4 -d scripts/tetra_vllm_tunnel.sh`: exit 0.
- `uv run --no-project python -m unittest discover -s scripts -p
  'test_ccr_fallback_policy.py' -v`: 45 tests, all OK.
- `uv run --no-project python scripts/quality.py`: all 12 checks passed
  (format, Python format/lint/typecheck, SLOC ratchet, ShellCheck, clippy, Rust
  tests, no-default-features, debug binary, Python helpers, and vendored GP
  routing).
- `git diff --check`: exit 0.

## Activation blocker

The deployed route was deliberately not changed. The installed policy remained
at SHA-256
`8cc2f4e4058d0ad3e0a8a4fea09e2a1e96062daf7ee0d7d3d86b68a5270d0f37`, and the
full governed preflight failed on three pre-existing routes:

- `deepseek`: `/models` returned 404 and `/messages` returned 402.
- `local27`: connection refused; ports 8080 and 8081 were not listening.
- `strata-b550`: connection reset by peer.

Z.AI, OpenRouter, SenseNova Vision, Meta, Holo4, and MiniMax model-list
preflights passed. Because route activation requires every currently routed
provider to pass full preflight, syncing the new final Tetra tier is blocked by
those existing failures, not by Tetra. The scratch qualification above is not a
substitute for that governed activation gate.

## Non-claims

- No production policy, runtime credential, tunnel LaunchAgent, listener binary,
  or listener restart was deployed.
- No quality benchmark, long-context test, concurrent-serving benchmark, or
  outage-replay test was run.
- Tetra remains Elliot's guest-owned service. CCR does not own, start, stop, or
  monitor the remote engine beyond consuming its loopback endpoint through SSH.
- A read-only delegated audit attempted before the parent qualification failed
  when all then-active CCR tiers failed; its partial report contained no
  implementation evidence and was not used as verification.
