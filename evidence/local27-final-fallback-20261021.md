# local27 final crawl-back tier, 2026-10-21

Agent: Claude Code (Sonnet 5.5), user-directed. Claim: the resident local
Qwen3.8-27B MXFP4 MLX server is the last shared fallback tier on every
governed listener.

## Observed server
- `dev.resmp.local27-mxfp4` (pid 39936, port 8081): `dflash_mlx.cli serve`,
  model `/Users/kearm/models/Qwen3.8-27B-MLX-GPTQ-MXFP4`, DFlash draft, prefix
  cache, `--max-tokens 512` default, thinking disabled, context 262144.
- `dev.resmp.local27-admission` (pid 94560, port 8080): bounded proxy
  (`LOCAL27_MAX_BACKEND=4`); returns 400 for `stream:true`, so CCR targets 8081.
- Backend served chat, SSE streaming, and an OpenAI tool call directly.

## Change
- `scripts/ccr_fallback_policy.py`: chain gains `local27,qwen3.8-27b-mxfp4`;
  new shared provider `local27` (openai, `maxInflight` 4, `maxtoken` 16384 when
  missing); integrity pin now `9104a0d9...beb6`.
- Deployed copy `~/.local/share/ccr-routing/ccr_fallback_policy.py` replaced
  (backup `.bak-pre-local27-20261021`); `fallback-policy.json` regenerated
  (backup `.bak-20261021-pre-local27`); `CCR_LOCAL27_API_KEY` placeholder
  (`local-no-auth`, server has no auth) added to `runtime-credentials.json`.

## Evidence
- `python3 -m unittest test_ccr_fallback_policy` (in `scripts/`): 35 tests OK.
- `ccr-fallback-policy check` and `preflight`: ok, local27 probe status 200.
- `sync`, then idle-gated (`ccr-wait-idle`) `launchctl kickstart -k` of
  ccr-rust (3456), ccr-rust-auth (3461), ccr-glm-workers (3457), ccr-ocr (3458).
- Pinned `local27,qwen3.8-27b-mxfp4` round trips through CCR: non-stream
  (3457), SSE stream (3456), tool_use (3461); all returned the local model id.

## Non-claims
- Not proven: behavior when every cloud tier actually fails (tier reached only
  by pinning); long-context agent quality; throughput under 4 concurrent
  requests; Codex Responses-path requests omitting `max_tokens`.
- Backend 8081 is shared with direct :8080 users; CCR traffic bypasses the
  proxy's admission counters (bounded by `maxInflight`, not by the proxy).
