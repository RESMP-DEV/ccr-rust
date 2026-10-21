# B550 Strata fallback, 2026-10-21

Agent: Codex, user-directed. Claim: the keyed B550 Strata Coder service is a
single-flight local coding fallback ahead of `local27`, reached through a
localhost SSH tunnel that does not pin B550's LAN address.

## Remote service

- B550 identity was re-authenticated through router DNS plus SSH host identity:
  host `b550`, `/etc/machine-id` SHA-256
  `1ea10d96153c065295848b9fea43a3a26619fa5269d5064ca971dc4444b4ae91`.
- User systemd unit `strata-ccr.service` is enabled with linger and listens on
  `0.0.0.0:18080`. The unit hash is
  `d542b0362b418d43591755ac149653fa5424c1aae233a5398c42a220e8180177`.
- The service config hash is
  `c6eef3a3975a7a607ba63aad5a7612a0ebc93a3e51e15b068976ac8aae9f4ddc`; the
  file is mode 0600 and contains the API credential, which is not reproduced
  here.
- Engine source is Strata revision `83f67c4fff7262ec4af6000f3e7d973db5f21dc0`;
  binary SHA-256 is
  `c10370e122a26752d7dbfa4802a4ff861f84c5806b54de95cdbb2b1ed86bfdf8`.
- The model is the existing dual-GPU Coder runtime: 23.42 GiB of experts,
  a 14.23 GiB VRAM cache, context 16384, direct PLE I/O, and one FIFO sequence.
  After warm load, GPU 0 used 18,726 MiB and GPU 1 used 22,832 MiB.
- Cold start loaded experts in 429 s at 0.60 GiB/s. This is an observation from
  the first service start after page-cache eviction, not a controlled load-time
  benchmark and not the earlier cached 53 s run.

## Live API checks

Direct router-DNS IPv4 access authenticated and completed:

- Nonstreaming with `reasoning_effort: none` returned
  `STRATA_B550_NO_THINKING_OK`, no `reasoning_content`, HTTP 200.
- SSE returned `STRATA_B550_STREAM_OK`, no reasoning delta, HTTP 200, and ended
  with `[DONE]`.
- A forced OpenAI `get_time` tool call returned one valid function call with
  arguments `{"timezone":"Etc/UTC"}`, HTTP 200.

macOS `b550.local` resolution flapped during validation, so CCR does not depend
on it directly. `deploy/com.kearm.strata-b550-tunnel.plist` runs an SSH forward
from `127.0.0.1:18081` through `b550-jump`; that SSH route resolves `b550.lan`
through the live router and authenticates B550's host key. The installed agent
reached `loaded: true` and completed `STRATA_B550_TUNNEL_OK` through the tunnel.

## Policy

The governed shared order is now MiniMax M3.1 Flash, GLM-5.3 Flash, DeepSeek
Flash, `strata-b550,qwen3.8-flash-next-coder`, and local27. The Strata provider
uses OpenAI protocol, the environment-backed `CCR_STRATA_B550_API_KEY`,
`maxInflight: 1`, `force_reasoning_effort: none`, and a missing-token default of
4096. local27 remains final because its context is 262144 tokens while this
Strata config is 16384.

Validation:

- `python3 -m unittest test_ccr_fallback_policy -v`: 36 tests OK.
- `plutil -lint deploy/com.kearm.strata-b550-tunnel.plist`: OK.
- `python3 scripts/quality.py`: PASS (format, Python format/lint/typecheck,
  SLOC ratchet, ShellCheck, clippy, Rust tests, no-default-features, debug
  binary, Python helpers, vendored GP routing).
- Source-policy preflight authenticated every routed provider: Z.AI, DeepSeek,
  local27, Strata B550, and MiniMax. GMI and Qwen were skipped as
  registered-but-unrouted.
- Deployed `sync` changed all four governed consumers; `check` reported no
  drift and policy SHA-256
  `fc2f0bb724d044c4708cc3f5c869d62ccfff7836047a1cd1c0128c455d73c8a1`.

## Non-claims

- No claim that the Strata tier is reached by an actual cloud-wide outage; the
  final acceptance below pins the route explicitly.
- No quality evaluation, long-context evaluation, throughput benchmark, or
  concurrent-serving benchmark was run. `maxInflight: 1` follows the current
  FIFO engine contract rather than a measured admission optimum.
- The B550 service is intentionally resident. This does not establish that both
  GPUs remain free for later CUDA benchmark arms.
- The tunnel depends on B550 and the Strix jump host being reachable; local27
  remains the final crawl-back when that lane is unavailable.
