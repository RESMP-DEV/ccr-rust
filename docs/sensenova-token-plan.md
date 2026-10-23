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
      "tier_name": "ccr-sensenova"
    }
  ],
  "Router": {
    "default": "sensenova,sensenova-6.8-flash-lite",
    "tiers": ["sensenova,sensenova-6.8-flash-lite"]
  }
}
```

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
Leave the control unset until the route is live-qualified.

The public beta advertises 1,500 calls per model per rolling five hours. The
API documentation additionally describes 60,000 points per rolling five hours
and 600,000 points per rolling seven days across its General and Flash-Lite
point classes. Treat those as plan limits, not per-route concurrency: no
SenseNova rate-limit headers were observed on the invalid-key probe.

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

## Current status and non-claims

The 2026-10-23 integration is credential-safe but not live-qualified: both
hosts were probed without a valid key and returned HTTP 401 JSON errors, which
proves endpoint and error reachability but not completion, tool-call, vision,
or streaming behavior. The public streaming excerpt shows a usage-only final
event with an empty `choices` array; confirm with one real stream that CCR
records its exact usage rather than relying on the excerpt's abbreviated
envelope. SenseNova remains registered-but-unrouted in the governed machine
policy until a real key completes the matrix above.
