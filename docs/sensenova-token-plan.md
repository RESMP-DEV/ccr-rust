# SenseNova Token Plan integration

CCR-Rust can route OpenAI-compatible clients through SenseNova's Token Plan
using the existing Chat Completions protocol. The provider needs no CCR
transformer: the upstream accepts the normal JSON body and Bearer
authorization, while CCR preserves its `reasoning` field as
`reasoning_content`.

## Provider configuration

Use the endpoint shown by the console that issued the key. The current M4
console uses the `.ai` host:

```json
{
  "Providers": [
    {
      "name": "sensenova",
      "api_base_url": "https://token.sensenova.ai/v1",
      "api_key": "${CCR_SENSENOVA_API_KEY}",
      "protocol": "openai",
      "models": ["sensenova-6.8-flash-lite"],
      "tier_name": "ccr-sensenova",
      "force_adaptive_thinking": true,
      "force_reasoning_effort": "none"
    }
  ],
  "Router": {
    "default": "sensenova,sensenova-6.8-flash-lite",
    "tiers": ["sensenova,sensenova-6.8-flash-lite"]
  }
}
```

SenseNova is deliberately outside the generic worker/code-review chain. The
governed policy registers a dedicated
`sensenova-vision,sensenova-6.8-flash-lite` capability with
`force_adaptive_thinking: true` and `force_reasoning_effort: none` as a
direct-only seat on the worker listener: the provider is present in `Providers`
but absent from `Router.tiers`, so only an explicit
`provider,model` request can use it. Generic worker fallback and the listener
named `ocr` (open code review, not image OCR) never consume it.

Load the credential into the router process, not into the checked-in config:

```bash
export CCR_SENSENOVA_API_KEY=sk-your-sensenova-key
```

The public documentation also uses `https://token.sensenova.cn/v1`. On
2026-10-23 both hosts returned the same JSON authentication errors for an
invalid key, so choose the host associated with the issuing console rather
than rewriting a key between regions.

## Protocol behavior

- Requests are OpenAI Chat Completions calls to `/v1/chat/completions`.
- Authentication is `Authorization: Bearer <key>`.
- `sensenova-6.8-flash-lite` is the exact preview model identifier.
- The authenticated 2026-10-23 catalog also listed `sensenova-u1.5-lite`,
  `sensenova-u1-fast`, and `sensenova-6.7-flash-lite`, but only
  `sensenova-u1.5-lite` completed an image generation: `u1-fast` returned 404,
  `6.7-flash-lite` returned 404 for chat, and docs-only `u1.5-fast` returned
  403 as unavailable to this plan. The governed policy therefore registers only
  the qualified chat model.
- Non-streaming responses may include `choices[0].message.reasoning`; CCR
  accepts that field as the generic `reasoning_content` alias and does not
  fabricate an Anthropic thinking signature.
- Streaming deltas may use `choices[0].delta.reasoning`; the same alias applies.
- `finish_reason: "length"` maps to Anthropic `stop_reason: "max_tokens"`.
- Token usage follows the OpenAI shape, including
  `prompt_tokens_details.cached_tokens` for cache-read accounting.

SenseNova documents `reasoning_effort: low | medium | high | max | none`,
with `high` as its default. CCR additionally accepts `minimal` and `xhigh`
for other providers, but SenseNova does not document those labels, so an
unsupported forced value can produce a deterministic upstream validation error.
The live API rejected the documented string form of `thinking: "disabled"` and
kept reasoning when only `reasoning_effort: none` was sent. It emitted no
reasoning only when `thinking` was the object `{"type":"adaptive"}` and
`reasoning_effort` was `none`; CCR's `force_adaptive_thinking` override automates
that pair. Remove the override only when a lane specifically needs deep
reasoning.

The public beta advertises 1,500 calls per model per rolling five hours. The
API documentation additionally describes 60,000 points per rolling five hours
and 600,000 points per rolling seven days across its General and Flash-Lite
point classes. Treat those as plan limits, not per-route concurrency: no
SenseNova rate-limit headers were observed on the invalid-key probe.

Flash-Lite calls drain the dedicated pool first, then spill into the general
pool. Every dedicated point earns a General-point rebate, while U-series image
generation spends only General points. This makes Flash-Lite the economical
text/vision lane and reserves U-series capacity for visual deliverables.

## Verification

After loading the key, validate the config and start a scratch listener:

```bash
ccr-rust --config examples/sensenova.json validate
ccr-rust --config examples/sensenova.json start
```

Then run the four-case chat/responses, stream/non-stream matrix:

```bash
python3 scripts/ccr_provider_smoke.py \
  --models sensenova,sensenova-6.8-flash-lite \
  --prompt 'Reply with SENSENOVA_OK.' --verbose
```

Inspect the response model and `x-ccr-tier` header or listener metrics to prove
the explicit SenseNova route served the request. A provider/model pin still
follows the configured retry policy; it does not isolate the account from other
tiers.

Keep output caps generous: the preview model thinks by default and consumed an
entire 128-token cap before emitting text. A 512-token cap completed the short
marker probe, while 1024 tokens provided comfortable headroom.

## Current status and non-claims

The 2026-10-23 qualification completed after the real key was loaded: the
authenticated catalog returned four IDs, the four-case chat/responses matrix
passed, the served model matched `sensenova-6.8-flash-lite`, and native tool
calls survived the CCR round trip. A live stream showed that its terminal
usage-only event retains `id`, `object`, `created`, and `model`, so CCR keeps
the exact usage rather than estimating it.

The measured fit is tool-less extraction, summarization, and image
understanding, not code review or long-horizon worker fallback. Non-thinking
text produced 164-200 output tokens in 4.1-4.9 s; image extraction produced 72
output tokens in 1.6-1.9 s. Tool-bearing Codex worker traffic is structurally a
bad fit: the upstream accepts only `function` and `custom` tools, while Codex
sends built-in tool types, producing deterministic 400s. The generic fallback
seat added earlier was therefore removed. Visual generation is a separate U1
API: `sensenova-u1.5-lite` produced a legible 512x512 infographic in 13.1 s,
while the advertised fast IDs were either stale (404) or not entitled at this
account (403). Quota exhaustion and sustained concurrency remain unmeasured.
