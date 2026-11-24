# NVIDIA Switchyard Integration Assessment — 2026-11-18

## Scope

This assessment determines whether NVIDIA NeMo Switchyard should be integrated into `ccr-rust` now and which boundary is appropriate. It combines primary-source research, a read-only audit of the current `ccr-rust` checkout, a pinned live compatibility probe, and a bounded architecture judgment review.

No `ccr-rust` source, tracked configuration, credential, remote, installed listener, or service definition was changed for this assessment. The live probe used ignored artifacts under `build/switchyard-probe/` and stopped every process it spawned.

## Verdict

**Do not embed Switchyard into `ccr-rust` now.** The immediately useful integration is already available without Rust changes: run a pinned Switchyard instance as an opt-in OpenAI-compatible upstream and configure a normal `protocol=openai` provider. That path is reversible and was exercised end to end against a mock model provider.

Treat that sidecar path as evaluation only. NVIDIA's own README says the standalone server is “provided only for demos and local evaluation” and “is not a supported production deployment path.” The production-grade boundary would be embedding the Rust `libsy` decision core while `ccr-rust` retains transport, credentials, admission, retries, and rate-limit state, but the public API is explicitly pre-1.0 and currently churning. Defer that integration until the gates below are met.

## Product and maturity findings

- Canonical project: `NVIDIA-NeMo/Switchyard`, Apache-2.0, created 2026-05-19 and actively maintained as of 2026-11-18.
- Latest stable release: v0.3.0, published 2026-09-22. Tag `v0.3.0` is commit `336196f6fbfc97ddc71c1700f6092e564e9f23c2`. A v0.4.0a1 prerelease and v0.4.0 release candidates exist.
- The documented primary integration is embedding `libsy` in a gateway: the host owns model clients, credentials, and service lifecycle; Switchyard supplies routing decisions.
- The stable server accepts OpenAI Chat Completions, OpenAI Responses, and Anthropic Messages. Its exposed routes include `/v1/chat/completions`, `/v1/responses`, `/v1/messages`, `/v1/models`, `/v1/stats`, `/metrics`, and `/health`.
- v0.3.0 is a breaking library/Python-CLI upgrade. The current unreleased changelog contains additional Rust source-breaking changes. The README explicitly says APIs, configuration, and routing behavior can change before v1.0 and requires pinning the integrated version.
- The Python wheel is not a server distribution. `nemo-switchyard` 0.3.0 ships the Python native extension; `switchyard-server` must be installed as the pinned Rust crate.
- Switchyard's benchmark evidence is mixed rather than uniformly favorable. Its checked-in DeepSWE results include routing configurations that cost less but solve fewer tasks than the stronger single model, and one advisor configuration scores below both single-model baselines. LangChain's reported 74% saving also came with an accuracy reduction and a judge cost equal to 21.2% of routed spend.

## Verified `ccr-rust` integration paths

### 1. Config-only upstream: use now for opt-in evaluation

The current provider model already covers the tested surface:

```json
{
  "name": "switchyard",
  "api_base_url": "http://127.0.0.1:4900/v1",
  "api_key": "${SWITCHYARD_API_KEY}",
  "protocol": "openai",
  "models": ["switchyard/default"]
}
```

The model listed by `ccr-rust` must be the Switchyard route ID. Switchyard translates that route ID to the configured upstream target model ID before calling the model provider.

Current source behavior:

- `src/config/types.rs` defaults `ProviderProtocol` to OpenAI and supports `openai`, `anthropic`, and `responses`.
- `src/router/dispatch.rs:480-489` appends `/chat/completions` to an OpenAI provider base unless the base already ends with that path.
- `src/router/dispatch.rs:512-569` sends Bearer authentication and permits provider `extra_headers` to override or add headers.
- Existing routing, admission, sticky-session, retry, and 429 state treats the Switchyard provider as one `provider,model` tier.

This path requires no transformer or new protocol variant. It should remain explicitly opt-in and must not be added to the governed shared fallback policy as a production tier.

### 2. Transformer normalization: conditional, with a streaming prerequisite

A Switchyard-specific transformer is warranted only if request or response fields need normalization beyond the existing OpenAI translation. A transformer must be registered in both registry construction paths.

The current checkout has an important limitation: in `src/router/streaming.rs:261-292`, an OpenAI-protocol stream applies a response transformer to a synthetic aggregate for tracing, but does not rewrite outbound SSE frames. A response-normalizing transformer therefore works for the non-streaming probe while silently leaving streaming client frames unchanged. Any production normalization requirement must first repair that streaming path and test both modes.

Adding a transformer also disables the Codex OpenAI passthrough fast path, forcing a round trip through the internal representation and increasing field-loss risk.

### 3. New provider protocol: not justified

Switchyard's tested stable server already speaks Chat Completions at the standard `/v1/chat/completions` path. A `ProviderProtocol` variant or custom URL builder would add maintenance cost without a demonstrated compatibility gap.

### 4. Embedded `libsy`: defer, then keep it advisory

The eventual production design should not make Switchyard a second autonomous router. `ccr-rust` must remain the single owner of tier eligibility, admission, retries, 429 backoff, and pinned-prefix semantics. `libsy` should at most provide a stage-aware score or candidate recommendation that `ccr-rust` applies to its already eligible route set.

Required gates:

1. A stable or contract-frozen released `libsy` API, including the decision-call API used by the selected algorithm, with no immediate source-breaking changes queued.
2. An adapter that preserves `pinned_prefix_len`, AIMD admission, per-route 429/backoff state, and the Codex passthrough fast path.
3. Cache-aware replay or A/B results on real Codex/Claude Code traces that show net quality and cost benefit after prompt-cache loss and all judge/model calls.
4. Streaming and non-streaming regression coverage in `ccr-rust`.
5. A pinned reviewed dependency version and local vendoring/licensing review before adding the crate.

## Live compatibility probe

The probe built and ran the exact stable server release, not `main`:

```bash
CARGO_HOME="$PWD/build/switchyard-probe/cargo-home" \
  cargo install --root "$PWD/build/switchyard-probe/toolchain" \
  --locked --version '=0.3.0' switchyard-server
```

Pins and identity:

- `switchyard-server` 0.3.0.
- Source tag `v0.3.0` commit `336196f6fbfc97ddc71c1700f6092e564e9f23c2`.
- Binary SHA-256 `f08e9c8623448d187fb68bdbcb22ac5b8ab25e1304e8039d090b6a6c0ab46ecd`.
- Installed `ccr-rust` 1.3.0 release binary, not the current dirty working tree.

The isolated chain used ports 4899 (mock OpenAI upstream), 4900 (Switchyard), 4901 (`ccr-rust` local listener), and 4902 (`ccr-rust` authenticated listener).

Passing checks:

- Switchyard TOML dry-run validation: exit 0, `server OK: switchyard/default`.
- Switchyard `/health`: HTTP 200, `{"status":"ok"}`.
- Switchyard `/v1/models`: HTTP 200, advertising route `switchyard/default` and streaming capability.
- Direct Switchyard non-streaming chat: HTTP 200 through a passthrough route to the mock target.
- `ccr-rust --config ... validate`: exit 0 with one provider and tier `switchyard,switchyard/default`.
- `ccr-rust /v1/models`: HTTP 200.
- Full chain non-streaming request: HTTP 200, content `mock-upstream-ok`, through `ccr-rust -> Switchyard -> mock upstream`.
- Authenticated `ccr-rust` listener with the probe key: HTTP 200; without the key: HTTP 401.
- Direct and chained streaming: HTTP 200 with valid SSE chunks and `data: [DONE]` after the mock was corrected to speak SSE.

The first streaming attempt returned 503 because the initial mock answered `stream:true` with plain JSON. Switchyard reported `openai_chat stream ended before a terminal event`; after the mock emitted real SSE, both direct and chained streaming passed. This was a mock artifact, not a Switchyard or `ccr-rust` defect.

All probe processes were stopped. Ports 4899, 4900, 4901, and 4902 were verified free afterward. The pre-existing listeners on 3456, 3457, 3458, 3459, and 3461 retained their original PIDs. Probe-local Cargo state was used; no Switchyard binary or crate was installed into user-level Cargo state.

## Primary sources and receipts

- NVIDIA repository and README: https://github.com/NVIDIA-NeMo/Switchyard
- v0.3.0 release: https://github.com/NVIDIA-NeMo/Switchyard/releases/tag/v0.3.0
- Switchyard server documentation and TOML schema in the tagged tree.
- NVIDIA technical announcement: https://developer.nvidia.com/blog/route-ai-agent-workloads-across-models-with-nvidia-nemo-switchyard/
- Primary-source research worker: `20261118-173923-pda6gldf`, final report at `/Users/kearm/.cache/ccr-worker-runs/20261118-173923-pda6gldf/final.txt`.
- Current-checkout integration audit worker: `20261118-173932-2s82atti`, final report at `/Users/kearm/.cache/ccr-worker-runs/20261118-173932-2s82atti/final.txt`.
- Live probe worker: `20261118-175009-ojp9t65a`, final report at `/Users/kearm/.cache/ccr-worker-runs/20261118-175009-ojp9t65a/final.txt`.
- Live probe artifacts: `build/switchyard-probe/report.md` and `build/switchyard-probe/receipt.json`, with 25 supporting files in that ignored probe directory.
- Judgment review: Gemini 4 Argon High conversation `581e58d0-6017-4ebc-95d9-570c1af22b88`, recommending `defer_embedding`.

## Non-claims

- The live chain used a local mock model provider. It does not prove behavior with OpenRouter, Anthropic, Z.AI, MiniMax, vLLM, or any real billable provider.
- It exercised only the OpenAI Chat Completions surface and Switchyard `passthrough` route. It did not test Anthropic Messages, OpenAI Responses, classifier, escalation, stage, composite, advisor, or prefill routing.
- It did not test `ccr-rust` multi-tier fallback, GP reranking, admission saturation, real 429 headers, or scale.
- It did not perform a Claude Code or Codex client tool round trip; the client was `curl`.
- It did not evaluate trademark, patent, NVIDIA brand-use, or hosted OpenRouter commercial terms.
- No benchmark result was reproduced. All cited benchmark figures remain NVIDIA, LangChain, or Switchyard-published results.
