# Kimi (ccr-kimi) Setup

ccr-kimi routes through ccr-rust to Moonshot's **Kimi K2.5** model using the
native Anthropic protocol at `api.kimi.com/coding/v1`.

## Prerequisites

- ccr-rust running (`ccr-rust status`)
- Moonshot API key (`KIMI_API_KEY`)

## Configuration

Add to `~/.claude-code-router/config.json`:

```json
{
  "Providers": [
    {
      "name": "kimi",
      "api_base_url": "https://api.kimi.com/coding/v1",
      "api_key": "${KIMI_API_KEY}",
      "models": ["kimi-k2.5", "kimi-k2-thinking"],
      "protocol": "anthropic",
      "tier_name": "ccr-kimi"
    }
  ]
}
```

Export your key and start ccr-rust:

```bash
export KIMI_API_KEY="your-moonshot-api-key"
ccr-rust start
```

## Verification

Check the route is exposed:

```bash
curl http://127.0.0.1:3456/v1/models | jq '.data[].id' | grep kimi
```

Send a test request:

```bash
curl -X POST http://127.0.0.1:3456/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer test" \
  -d '{
    "model": "kimi,kimi-k2.5",
    "messages": [{"role": "user", "content": "Hello"}],
    "max_tokens": 50
  }'
```

Check latency metrics:

```bash
curl http://127.0.0.1:3456/v1/latencies
```

## How ccr-kimi Integrates with Claude Code

Point Claude Code at your local CCR-Rust instance:

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:3456
claude
```

CCR-Rust then routes requests to the configured `kimi` provider using the
native Anthropic protocol.
