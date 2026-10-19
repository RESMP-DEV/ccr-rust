# OpenRouter setup

OpenRouter is an OpenAI-compatible provider. CCR needs no per-model adapter
for a standard OpenRouter Chat Completions model: the `anthropic` transformer
converts Anthropic client requests to OpenAI Chat format, and the OpenAI
frontend can use the provider directly. The `openrouter` transformer entry is
an explicit compatibility marker; the router also adds OpenRouter attribution
headers automatically when the provider name or API host identifies OpenRouter.

## Current free-inference example

Use [`examples/openrouter-free.json`](../examples/openrouter-free.json):

```bash
export OPENROUTER_API_KEY="sk-or-..."
ccr-rust --config examples/openrouter-free.json validate
ccr-rust --config examples/openrouter-free.json start
```

The catalog check on 2026-10-19 found these zero-price text/code models:

| Route | Context | Catalog notes |
|---|---:|---|
| `openrouter,openrouter/free` | 200k | OpenRouter's free-model router |
| `openrouter,minimax/minimax-m3.1-flash:free` | 1M | M3.1 Flash, agentic/coding-oriented |
| `openrouter,xingchen-agi/xing4.0-29b-a4b:free` | 262k | XingChen Xing4.0 29B A4B |
| `openrouter,nvidia/nemotron-3.5-super-vl:free` | 1M | multimodal text/image/document/video model |
| `openrouter,nvidia/nemotron-3.5-lightning:free` | 1M | 30B MoE, high-throughput lane |
| `openrouter,cohere/north-mini-code:free` | 256k | Cohere agentic coding model |
| `openrouter,poolside/laguna-xs-2.1:free` | 262k | Poolside coding-agent model |
| `openrouter,thinkingmachines/inkling-small:free` | 1M | Inkling Small multimodal MoE |

OpenRouter's free inference allowance is account-wide, not per model. A daily
429 from `openrouter_free_tier_daily` means every free model on that account is
temporarily capped; explicit fallback tiers help with model-specific upstream
failures, not with the account-wide allowance.

Some free endpoints gate requests that do not look like an agentic harness.
The example carries a Claude CLI `User-Agent` because CCR builds fresh upstream
headers rather than forwarding the inbound client agent. If you use Codex,
replace it with your actual Codex CLI agent identifier. Do not present the
router itself as a coding agent to bypass a provider policy.

## Verify an exact model

A model listing is not acceptance. After starting the router, check:

```bash
curl -sS http://127.0.0.1:3456/v1/models | jq '.data[].id'
curl -sS http://127.0.0.1:3456/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "openrouter,minimax/minimax-m3.1-flash:free",
    "max_tokens": 512,
    "messages": [{"role": "user", "content": "Reply exactly OPENROUTER_OK."}]
  }'
```

Confirm the response model, finish reason, and usage. For a coding client,
also run a harmless tool round trip. OpenRouter advertises tool support for
the explicit models above, but provider behavior can differ by model and
harness; CCR does not silently repair a provider that accepts a tool schema but
returns an unusable tool call.

## Compatibility boundaries

- OpenRouter standard models use the OpenAI Chat protocol.
- The `anthropic` transformer handles Anthropic-to-OpenAI request conversion.
- No model-specific OpenRouter adapter is required for standard Chat models.
- Multimodal support depends on the selected upstream model and the client's
  input shape; test one image/document before relying on it.
- Structured-output constraints are protocol-specific and may not survive a
  cross-protocol fallback. Keep schema-critical requests on a provider/model
  that supports the required format.
