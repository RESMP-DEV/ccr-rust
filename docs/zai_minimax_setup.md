# Z.AI and MiniMax Provider Setup

This guide covers setting up the modern Z.AI (GLM) and MiniMax providers for CCR-Rust, which are the recommended default fallback options when your Claude usage runs out.

## Z.AI (GLM) Setup

### Getting an API Key

1. Visit [Z.AI Open Platform](https://z.ai/model-api)
2. Sign up for an account (international users) or [Zhipu AI](https://open.bigmodel.cn/) (China users)
3. Navigate to the API Keys section
4. Generate a new API key

### Choosing a Plan

| Plan | Pricing | Models | Best For |
|------|---------|--------|----------|
| GLM Coding Plan | From $18/month | `glm-5.2`, `glm-5.1`, `glm-5-turbo` | Daily coding work, Claude Code/Cline |
| Token Plan | Pay-per-token | All GLM models | Variable workloads, testing |

The **Coding Plan** is recommended for Claude Code users - it uses prompt-based pricing instead of token counting.

### Configuration

Add to your `~/.claude-code-router/config.json`:

```json
{
  "Providers": [
    {
      "name": "zai",
      "api_base_url": "https://api.z.ai/api/coding/paas/v4",
      "api_key": "${ZAI_API_KEY}",
      "models": ["glm-5.2", "glm-5.1", "glm-5-turbo"],
      "transformer": {
        "use": ["anthropic", "glm"]
      },
      "tier_name": "ccr-glm"
    }
  ],
  "Router": {
    "default": "zai,glm-5.2"
  }
}
```

### Model Differences

| Model | Context | Features |
|-------|---------|----------|
| `glm-5.2` | 1M | Latest, `reasoning_effort` parameter |
| `glm-5.1` | 128K | Stable, reliable |
| `glm-5-turbo` | 128K | Faster responses |
| `glm-4.7` | 128K | Legacy model |

### Reasoning Effort Parameter

GLM-5.2 supports configurable reasoning depth:

- `low` - Faster, less detailed reasoning
- `medium` - Balanced (default)
- `high` - Detailed, thorough reasoning

To customize, add to your environment or config:

```bash
# Set via environment variable (affects all GLM-5.2 requests)
export ZAI_REASONING_EFFORT=high
```

Or add a preset:

```json
{
  "Presets": {
    "deep-reasoning": {
      "route": "zai,glm-5.2",
      "extra_params": {
        "reasoning_effort": "high"
      }
    }
  }
}
```

## MiniMax Setup

### Getting an API Key

1. Visit [MiniMax Platform](https://platform.minimax.io/)
2. Sign up for a Token Plan or Coding Plan
3. Navigate to API Keys and create a subscription key
4. Copy the key (format: `sk-cp-...`)

### Choosing a Plan

| Plan | Models | Best For |
|------|--------|----------|
| Token Plan | M3, M2.7, M2.5 | Flexible usage, pay-per-token |
| Coding Plan | M3, M2.7 | AI coding tools, flat monthly rate |

The **Token Plan** is recommended for most users.

### Configuration

Add to your `~/.claude-code-router/config.json`:

```json
{
  "Providers": [
    {
      "name": "minimax",
      "api_base_url": "https://api.minimax.io/anthropic/v1",
      "api_key": "${MINIMAX_API_KEY}",
      "models": [
        "MiniMax-M3",
        "MiniMax-M2.7",
        "MiniMax-M2.7-highspeed",
        "MiniMax-M2.5",
        "MiniMax-M2.5-highspeed"
      ],
      "transformer": {
        "use": ["minimax"]
      },
      "protocol": "anthropic",
      "tier_name": "ccr-mm"
    }
  ],
  "Router": {
    "default": "minimax,MiniMax-M3"
  }
}
```

### Model Differences

| Model | Context | Speed | Features |
|-------|---------|-------|----------|
| `MiniMax-M3` | 1M | ~60 tps | Native thinking blocks, multimodal |
| `MiniMax-M2.7` | 204K | ~60 tps | Agentic reasoning |
| `MiniMax-M2.7-highspeed` | 204K | ~100 tps | Faster M2.7 |
| `MiniMax-M2.5` | 204K | ~60 tps | Peak performance |
| `MiniMax-M2.5-highspeed` | 204K | ~100 tps | Faster M2.5 |

### Thinking Format

MiniMax-M3 uses native Anthropic-style `thinking` blocks:

```json
{
  "thinking": {
    "type": "adaptive"
  }
}
```

MiniMax-M2.x uses the `reasoning_split` format (handled automatically by the transformer).

## Complete Combined Setup

For optimal fallback routing, configure both providers:

```json
{
  "Providers": [
    {
      "name": "zai",
      "api_base_url": "https://api.z.ai/api/coding/paas/v4",
      "api_key": "${ZAI_API_KEY}",
      "models": ["glm-5.2", "glm-5.1", "glm-5-turbo"],
      "transformer": {
        "use": ["anthropic", "glm"]
      },
      "tier_name": "ccr-glm"
    },
    {
      "name": "minimax",
      "api_base_url": "https://api.minimax.io/anthropic/v1",
      "api_key": "${MINIMAX_API_KEY}",
      "models": [
        "MiniMax-M3",
        "MiniMax-M2.7",
        "MiniMax-M2.7-highspeed"
      ],
      "transformer": {
        "use": ["minimax"]
      },
      "protocol": "anthropic",
      "tier_name": "ccr-mm"
    }
  ],
  "Router": {
    "default": "zai,glm-5.2",
    "tiers": [
      "zai,glm-5.2",
      "zai,glm-5.1",
      "minimax,MiniMax-M3",
      "minimax,MiniMax-M2.7-highspeed"
    ]
  },
  "Presets": {
    "coding": {
      "route": "zai,glm-5.2",
      "temperature": 0.7
    },
    "fast": {
      "route": "minimax,MiniMax-M2.7-highspeed"
    }
  }
}
```

## Environment Variables

Add to your `.env` file or shell profile:

```bash
# Z.AI
export ZAI_API_KEY=your-zai-key-here
# Optional: ZAI_REASONING_EFFORT=high

# MiniMax
export MINIMAX_API_KEY=sk-cp-your-minimax-key-here
```

## Verification

Start CCR-Rust and verify:

```bash
ccr-rust start

# Verify the router is running
ccr-rust status

# Check configured models
curl http://localhost:3456/v1/models

# Test a request
curl http://localhost:3456/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "zai,glm-5.2",
    "messages": [{"role": "user", "content": "Hello!"}],
    "max_tokens": 100
  }'
```

## Troubleshooting

### Z.AI Issues

**Problem**: `reasoning_effort` parameter not recognized
- **Solution**: Ensure you're using GLM-5.2 or GLM-5.1. Older models don't support this parameter.

**Problem**: Rate limited (429) errors
- **Solution**: The Z.AI coding endpoint has rate limits. Set `"honor_ratelimit_headers": false` in the provider config if you see informational 429s.

### MiniMax Issues

**Problem**: Thinking blocks not appearing
- **Solution**: Ensure you're using the Anthropic-compatible endpoint (`/anthropic/v1`) with `protocol: "anthropic"` in the config.

**Problem**: Structured reasoning_content array
- **Solution**: The transformer automatically extracts reasoning from MiniMax-M3's structured format. No manual parsing needed.

## See Also

- [Configuration](configuration.md) — Full configuration schema
- [Claude Code Setup](claude_code_setup.md) — Using CCR-Rust with Claude Code
- [Observability](observability.md) — Monitoring and metrics
