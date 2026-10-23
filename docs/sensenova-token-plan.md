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

On the governed workstation, the reviewed shared chain places
`sensenova,sensenova-6.8-flash-lite` immediately after MiniMax and before the
Z.AI GLM Flash window. Main and auth still begin with their GLM-5.3 primary;
workers still begin with MiniMax. This spends the free SenseNova lane before
scarce quota, while retaining the normal 429 cascade.

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
  `sensenova-u1-fast`, and `sensenova-6.7-flash-lite`; the governed policy
  registers those IDs but routes only Flash Lite until separate lanes are
  qualified.
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

Keep output caps generous: the preview model thinks by default and consumed an
entire 128-token cap before emitting text. A 512-token cap completed the short
marker probe, while 1024 tokens provided comfortable headroom.

## Current status and non-claims

The 2026-10-23 qualification completed after the real key was loaded: the
authenticated catalog returned four model IDs, the four-case matrix above
passed, the served model matched `sensenova-6.8-flash-lite`, and a native
weather-tool request returned `get_weather` with `{"city":"Paris"}`. A live
stream also showed that its final usage-only event retains
`id`, `object`, `created`, and `model`, so CCR can parse the exact usage
instead of estimating it. Vision input and quota exhaustion remain unmeasured.
