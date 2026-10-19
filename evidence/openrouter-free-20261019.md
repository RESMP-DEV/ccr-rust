# OpenRouter free-inference catalog check — 2026-10-19

## Catalog evidence

The public `https://openrouter.ai/api/v1/models` catalog was read live and
filtered to models with both prompt and completion price exactly zero. It
contained 20 zero-price entries. The selected text/code candidates are:

| ID | Context | Created (UTC) | Tool support advertised |
|---|---:|---|---|
| `openrouter/free` | 200k | 2026-02-01 | router-managed |
| `minimax/minimax-m3.1-flash:free` | 1M | 2026-10-19 | yes |
| `xingchen-agi/xing4.0-29b-a4b:free` | 262k | 2026-10-16 | yes |
| `nvidia/nemotron-3.5-super-vl:free` | 1M | 2026-10-15 | yes |
| `nvidia/nemotron-3.5-lightning:free` | 1M | 2026-08-11 | yes |
| `cohere/north-mini-code:free` | 256k | 2026-06-17 | yes |
| `poolside/laguna-xs-2.1:free` | 262k | 2026-07-02 | yes |
| `thinkingmachines/inkling-small:free` | 1M | 2026-07-30 | yes |

The complete catalog response was retained privately at
`/tmp/openrouter-free-models.json`; it is not copied into the repository.

## Live completion evidence and limit

Direct OpenRouter Chat probes used the existing private account key, a Claude
CLI User-Agent, `max_tokens:256`, and an exact-answer prompt.

- `minimax/minimax-m3.1-flash:free` → HTTP 200, response model echoed exactly,
  `MINIMAX-M3.1-FLASH:FREE_OK`, cost 0.
- `thinkingmachines/inkling-small:free` → HTTP 200, response model echoed
  exactly, `INKLING-SMALL:FREE_OK.`, cost 0.

The next probes returned account-wide 429:
`Rate limit exceeded: free-models-per-day-high-balance`, limit 1000, remaining
0, reset 2026-10-20T00:00Z. Probes stopped immediately at that explicit limit.
Therefore Xing4.0, Nemotron 3.5 Super VL/Lightning, North Mini Code, Laguna XS,
and the OpenRouter free router are catalog- and schema-verified but did not
receive live completions in this account window. No tool-call acceptance is
claimed for any new route.

## Repository state assessment

Generic end-user support:

- The router accepts Anthropic Messages, OpenAI Chat, and OpenAI Responses on
  its local frontends.
- Upstream providers can be configured as OpenAI Chat, Anthropic Messages, or
  OpenAI Responses.
- Standard OpenAI-compatible upstreams are config-only; no per-model adapter is
  required when the provider implements the selected protocol and tool schema.
- The OpenRouter provider uses the OpenAI Chat protocol. For Anthropic clients,
  the `anthropic` transformer converts the request to OpenAI Chat; the
  `openrouter` transformer is an explicit identity compatibility marker.
- CCR automatically adds OpenRouter attribution headers when the provider name
  or host identifies OpenRouter. A provider-level User-Agent remains necessary
  for free endpoints that gate non-agent clients.

Current generic-support gaps:

- Model catalogs are static examples. CCR does not discover or auto-refresh an
  OpenRouter account catalog.
- There is no automatic per-model tool-loop qualification; users must run the
  documented representative client round trip.
- Provider-specific normalization is manual and uneven: GLM, MiniMax, Kimi,
  DeepSeek, and LongCam have named transformers; standard OpenRouter models do
  not need one, but other nonstandard upstreams may.
- The runtime still uses the legacy transformer registry in
  `src/transformer/mod.rs`; the newer factory registry under
  `src/transform/registry.rs` is not the server construction path and does not
  register OpenRouter. This is technical debt, not an OpenRouter runtime defect.
- The workstation policy, fixed listener consumers, credentials, ports, and
  recovery tooling are machine-specific overlays and are not required by a
  standalone user.
- Configuration changes require a listener restart; CCR has no in-process
  config hot reload.

## Validation

- `jq empty examples/openrouter-free.json` — exit 0.
- `OPENROUTER_API_KEY=dummy ccr-rust --config
  examples/openrouter-free.json validate` — valid; one provider, eight models,
  and eight tiers.
- `python3 scripts/quality.py` — PASS for format, clippy, rust-tests,
  no-default-features, python-helpers, and vendored-gp-routing; receipt:
  `build/reports/quality/receipt.json`.
