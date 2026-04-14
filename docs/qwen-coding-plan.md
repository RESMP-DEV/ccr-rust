# Qwen Coding Plan Integration

CCR-Rust supports Alibaba Cloud Model Studio's **Coding Plan** subscription, enabling access to Qwen's latest code-optimized models at predictable monthly costs.

## Overview

Coding Plan is an AI coding subscription service from Alibaba Cloud Model Studio. It provides:

- **Latest Qwen models**: `qwen3-coder-next` (latest coding-focused model), `qwen3.5-plus` (recommended general model)
- **Fixed monthly pricing**: Pro at $50/month; Lite is legacy-only and no longer accepts new subscriptions
- **Monthly quota**: Up to 90,000 requests per month on Pro
- **Compatible with**: Claude Code, Cline, Qwen Code, OpenClaw

## Supported Models

| Model | Best For |
|-------|----------|
| `qwen3-coder-next` | Code generation, refactoring, tool calling |
| `qwen3.5-plus` | General coding assistance, reasoning, and longer planning |

## Getting Started

### 1. Subscribe to Coding Plan

1. Go to the [Coding Plan subscription page](https://common-buy-intl.alibabacloud.com/?commodityCode=sfm_codingplan_public_intl)
2. Select Lite ($10/month) or Pro ($50/month)
3. Complete payment

> **Note:** Only Alibaba Cloud accounts can subscribe. RAM users are not supported.

### 2. Get Your Coding Plan API Key

1. Go to the [Coding Plan Console](https://modelstudio.console.alibabacloud.com/ap-southeast-1/?tab=globalset#/efm/coding_plan)
2. Copy your **plan-specific API key** (format: `sk-sp-xxxxx`)

> **Important:** This is different from the standard Model Studio API key (`sk-xxxxx`). The Coding Plan key uses a different endpoint and billing system.

### 3. Configure CCR-Rust

Add to your `.env` file:

```bash
# .env
QWEN_API_KEY=sk-sp-your-coding-plan-key
```

Add the provider to `~/.claude-code-router/config.json`:

```json
{
    "Providers": [
        {
            "name": "qwen",
            "api_base_url": "https://coding-intl.dashscope.aliyuncs.com/v1",
            "api_key": "${QWEN_API_KEY}",
            "models": ["qwen3-coder-next", "qwen3.5-plus"],
            "transformer": { "use": ["anthropic"] },
            "tier_name": "ccr-qwen"
        }
    ],
    "Router": {
        "tiers": [
            "zai,glm-5.1",
            "qwen,qwen3-coder-next",
            "minimax,MiniMax-M2.7",
            "deepseek,deepseek-reasoner"
        ]
    }
}
```

### 4. Restart CCR-Rust

Restart your running CCR-Rust process so it reloads the updated config. If you
run it in the foreground, stop it with `Ctrl+C` and start it again:

```bash
ccr-rust start
```

### 5. Verify Integration

```bash
curl -X POST http://localhost:3456/v1/messages \
  -H "Content-Type: application/json" \
  -H "anthropic-version: 2023-06-01" \
    -d '{"model":"qwen,qwen3-coder-next","max_tokens":50,"messages":[{"role":"user","content":"Hello"}]}'
```

## API Endpoints

Coding Plan uses different endpoints than standard DashScope:

| API Format | Coding Plan Endpoint |
|------------|---------------------|
| OpenAI-compatible | `https://coding-intl.dashscope.aliyuncs.com/v1` |
| Anthropic-compatible | `https://coding-intl.dashscope.aliyuncs.com/apps/anthropic` |

> **Do not confuse** with standard DashScope endpoints (`dashscope.aliyuncs.com` or `dashscope-intl.aliyuncs.com`).

## Routing Syntax

CCR-Rust uses `provider,model` syntax for direct routing:

```bash
# Route to Qwen Coder
curl -X POST http://localhost:3456/v1/messages \
  -H "Content-Type: application/json" \
    -d '{"model":"qwen,qwen3-coder-next",...}'

# Route to Qwen 3.5 Plus (reasoning / planning)
curl -X POST http://localhost:3456/v1/messages \
  -H "Content-Type: application/json" \
    -d '{"model":"qwen,qwen3.5-plus",...}'
```

## Preset Configuration

Create presets for common use cases:

```json
{
    "Presets": {
        "coding_qwen": {
            "route": "qwen,qwen3-coder-next",
            "temperature": 0.7
        },
        "reasoning_qwen": {
            "route": "qwen,qwen3.5-plus"
        }
    }
}
```

Use presets via the preset endpoint:

```bash
curl -X POST http://localhost:3456/preset/coding_qwen/v1/messages \
  -H "Content-Type: application/json" \
  -d '{"messages":[{"role":"user","content":"Refactor this function..."}]}'
```

## Quota Consumption

A single user question may trigger multiple model calls:

| Task Type | Typical Requests |
|-----------|------------------|
| Simple Q&A / code generation | 5-10 |
| Code refactoring / complex tasks | 10-30+ |

**Pro Quota (90,000/month):** Approximately 3,000-9,000 complex questions per month.

Check your usage in the [Coding Plan Console](https://modelstudio.console.alibabacloud.com/ap-southeast-1/?tab=globalset#/efm/coding_plan).

## Tier Placement

Recommended tier position for balanced routing:

```json
{
    "Router": {
        "tiers": [
            "zai,glm-5.1",           // Tier 0: Primary (Z.AI GLM-5.1)
            "qwen,qwen3-coder-next", // Tier 1: Qwen Coder
            "minimax,MiniMax-M2.7", // Tier 2: MiniMax
            "deepseek,deepseek-reasoner" // Tier 3: DeepSeek
        ]
    }
}
```

## Usage Restrictions

> **Important:** Coding Plan quota can only be used with interactive coding tools. Do not use it for:
> - API calls in automated scripts
> - Custom application backends
> - Non-interactive batch calling scenarios
>
> Violating these terms may result in subscription suspension or API key revocation.

## Troubleshooting

### Invalid API Key

```
Error: invalid_api_key
```

**Causes:**
- Using standard DashScope key (`sk-xxx`) instead of Coding Plan key (`sk-sp-xxx`)
- Using wrong endpoint (must be `coding-intl.dashscope.aliyuncs.com`)

**Fix:** Verify your key format and endpoint:

```bash
source .env && curl -X POST "https://coding-intl.dashscope.aliyuncs.com/v1/chat/completions" \
  -H "Authorization: Bearer $QWEN_API_KEY" \
  -H "Content-Type: application/json" \
    -d '{"model":"qwen3-coder-next","messages":[{"role":"user","content":"test"}]}'
```

### Not Routing to Qwen

If requests fall through to other tiers instead of Qwen:

1. Check the tier is in the config:
   ```bash
   jq '.Router.tiers' ~/.claude-code-router/config.json
   ```

2. Use explicit `provider,model` routing:
   ```json
    {"model": "qwen,qwen3-coder-next", ...}
   ```

3. Check the terminal or service logs where `ccr-rust` is running for
    `ccr-qwen` routing decisions.

### Quota Exceeded

```
Error: quota_exceeded
```

Your monthly/weekly/5-hour quota is exhausted. Check limits:

| Plan | 5-hour Limit | Weekly Limit | Monthly Limit |
|------|--------------|--------------|---------------|
| Lite | 1,200 | 9,000 | 18,000 |
| Pro | 6,000 | 45,000 | 90,000 |

Quotas reset automatically (5-hour: rolling, weekly: Monday 00:00 UTC+8, monthly: renewal date).

## Full Configuration Example

```json
{
    "Providers": [
        {
            "name": "qwen",
            "api_base_url": "https://coding-intl.dashscope.aliyuncs.com/v1",
            "api_key": "${QWEN_API_KEY}",
            "models": ["qwen3-coder-next", "qwen3.5-plus"],
            "transformer": { "use": ["anthropic"] },
            "tier_name": "ccr-qwen"
        }
    ],
    "Router": {
        "default": "zai,glm-5.1",
        "tiers": [
            "zai,glm-5.1",
            "qwen,qwen3-coder-next",
            "minimax,MiniMax-M2.7",
            "deepseek,deepseek-reasoner"
        ]
    },
    "Presets": {
        "coding_qwen": {
            "route": "qwen,qwen3-coder-next",
            "temperature": 0.7
        }
    }
}
```

## References

- [Coding Plan Overview](https://www.alibabacloud.com/help/en/model-studio/coding-plan)
- [Qwen Code with Coding Plan](https://www.alibabacloud.com/help/en/model-studio/qwen-code-coding-plan)
- [Qwen-Coder Model Capabilities](https://www.alibabacloud.com/help/en/model-studio/qwen-coder)
- [Model Studio Console](https://modelstudio.console.alibabacloud.com/)
