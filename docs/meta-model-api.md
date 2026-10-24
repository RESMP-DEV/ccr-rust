# Meta Model API (Muse) integration

CCR-Rust routes Anthropic-protocol traffic through Meta's Model API, the
first-party inference API for the Muse family. The Messages endpoint is a
native Anthropic-compatible adapter, so CCR reaches it with the same
provider path as MiniMax and DeepSeek: no transformer beyond the
output-limit injection.

## Provider configuration

```json
{
  "Providers": [
    {
      "name": "meta",
      "api_base_url": "https://api.meta.ai/v1",
      "api_key": "${CCR_META_API_KEY}",
      "protocol": "anthropic",
      "auth_header": "authorization",
      "models": ["muse-spark-1.3"],
      "transformer": {
        "use": [["maxtoken", {"max_tokens": 32768, "only_if_missing": true}]]
      }
    }
  ],
  "Router": {
    "default": "meta,muse-spark-1.3",
    "tiers": ["meta,muse-spark-1.3"],
    "strictTierOrder": true
  }
}
```

The governed policy registers `meta` as the last cloud fallback tier in the
shared chain (behind MiniMax, GLM-5.3 Flash, and DeepSeek, ahead of the
Strata B550 and local27 crawl-backs) for the main interactive, auth, and
worker lanes. `muse-spark-1.3` is the cheap standard tier; the catalog also
lists `muse-wonder-1.0` (the flagship, roughly 3x the price) plus
contributor-mode variants, which stay unregistered.

## Credentials

Keys come from the Model API dashboard and look like
`LLM_<numeric-id>_<secret>`. The `LLM_` prefix is part of the key: a key
pasted without it fails with `401 invalid_api_key`, which the error guide
documents as the malformed-key case. Store the key as `CCR_META_API_KEY` in
`~/.claude-code-router/runtime-credentials.json` (mode 0600); requests send
it as `Authorization: Bearer <key>`.

## Protocol notes

- **Muse always reasons.** Thinking cannot be disabled: Chat Completions
  rejects `reasoning_effort: "none"` with 400, and the Messages adapter
  returns reasoning as `redacted_thinking` blocks (opaque payloads, no
  deltas). CCR parses those blocks, keeps them faithful for Anthropic-surface
  clients, and drops them from OpenAI-surface serializations. Small output
  caps are consumed by thinking first: a 64-token cap produced 62 thinking
  tokens and no text, so the tier injects a generous `max_tokens` default
  (32768) when a request omits one.
- **`max_tokens` is required** on the Messages surface; the `maxtoken`
  transformer injects it only when missing, never raising a client's lower
  cap.
- **Unsupported fields 400**: `stop_sequences`, `top_k`, `container`, and
  `inference_geo` are rejected on the Messages adapter. CCR's translation
  does not emit them; a client that forces stop sequences through would fail
  on this tier and fall through to the next one.
- **Prompt caching is automatic**; live tool-loop traffic showed
  `cached_tokens` on the second turn with no configuration.
- Context window is 1,048,576 tokens; usage carries
  `output_tokens_details.thinking_tokens`.

## Verification

```bash
# credential + catalog
curl -sS -H "Authorization: Bearer $CCR_META_API_KEY" https://api.meta.ai/v1/models

# through a listener, explicit route
curl -sS -X POST http://127.0.0.1:3457/v1/chat/completions \
  -H 'Content-Type: application/json' -H 'Authorization: Bearer test' \
  -d '{"model":"meta,muse-spark-1.3","max_tokens":1024,
       "messages":[{"role":"user","content":"Reply with META_MUSE_OK."}]}'
```

The unauthenticated `GET /v1/status` endpoint answers
`{"is_alive":true,"service_status":"operational",...}` for reachability
checks without spending credits.

## Limitations

- Reasoning does not carry across turns through the Chat Completions
  surface, and CCR does not surface Muse reasoning items to Responses-surface
  clients, so multi-turn reasoning replay is unavailable on this tier.
- Rate limits are per team (Standard: 3,000 RPM / 4M TPM), shared across all
  keys on the account.
- `muse-wonder-1.0`, image generation (`muse-image-1.0`), voice
  transcription, and SAM are not registered; adding them is a config-only
  change once a use case exists.
