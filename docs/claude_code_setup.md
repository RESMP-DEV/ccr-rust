# Claude Code Setup Guide for CCR-Rust

This guide covers setting up Claude Code CLI to work with CCR-Rust as a proxy router, enabling intelligent routing to multiple LLM backends including Anthropic Claude, Z.AI GLM, DeepSeek Reasoner, and more.

## Table of Contents

1. [Installing Claude Code](#1-installing-claude-code)
2. [Configuring CCR-Rust for Claude Code](#2-configuring-ccr-rust-for-claude-code)
3. [Cache Control Settings](#3-cache-control-settings)
4. [Thinking Block Preferences](#4-thinking-block-preferences)
5. [Reasoning Provider Support](#reasoning-provider-support)
6. [Troubleshooting](#6-troubleshooting)

---

## 1. Installing Claude Code

Install the official Anthropic Claude Code CLI globally using npm:

```bash
npm install -g @anthropic-ai/claude-code
```

Verify the installation:

```bash
claude --version
```

Expected output:
```
0.2.x (or newer)
```

### System Requirements

- Node.js 18 or higher
- npm 8 or higher
- An Anthropic API key (or compatible endpoint via CCR-Rust)
- Git (for repository context)

### Alternative: Local Installation

If you prefer not to install globally:

```bash
# Using npx (no installation required)
npx @anthropic-ai/claude-code --version
```

### Post-Installation Setup

After installation, Claude Code may prompt for initial configuration on first run:

```bash
# Start Claude Code to complete setup
claude
```

Follow the interactive prompts to:
1. Accept the terms of service
2. Configure your API key (can be skipped if using CCR-Rust proxy)
3. Set default preferences

---

## 2. Configuring CCR-Rust for Claude Code

CCR-Rust provides an Anthropic-compatible endpoint at `/v1/messages` that Claude Code can use. This enables routing to multiple backends while maintaining full Claude Code functionality.

### 2.1 Create/Edit Configuration File

Edit your CCR-Rust configuration file (default: `~/.claude-code-router/config.json`):

```json
{
  "Providers": [
    {
      "name": "anthropic",
      "api_base_url": "https://api.anthropic.com/v1/messages",
      "api_key": "${ANTHROPIC_API_KEY}",
      "models": ["claude-3-5-sonnet-20241022", "claude-3-opus-20240229", "claude-3-5-haiku-20241022"]
    },
    {
      "name": "zai",
      "api_base_url": "https://api.z.ai/api/inference/v1/chat/completions",
      "api_key": "${ZAI_API_KEY}",
      "models": ["glm-5"],
      "transformer": { "use": ["openai_to_anthropic"] }
    },
    {
      "name": "deepseek",
      "api_base_url": "https://api.deepseek.com/chat/completions",
      "api_key": "${DEEPSEEK_API_KEY}",
      "models": ["deepseek-reasoner", "deepseek-chat"],
      "transformer": { "use": ["deepseek", "openai_to_anthropic"] }
    },
    {
      "name": "openrouter",
      "api_base_url": "https://openrouter.ai/api/v1/chat/completions",
      "api_key": "${OPENROUTER_API_KEY}",
      "models": ["minimax/minimax-m2.1", "google/gemini-2.5-pro-preview"],
      "transformer": { "use": ["openrouter", "openai_to_anthropic"] }
    }
  ],
  "Router": {
    "default": "anthropic,claude-3-5-sonnet-20241022",
    "think": "deepseek,deepseek-reasoner",
    "longContext": "openrouter,minimax/minimax-m2.1",
    "longContextThreshold": 1048576,
    "tierRetries": {
      "tier-0": {
        "max_retries": 5,
        "base_backoff_ms": 50,
        "backoff_multiplier": 1.5,
        "max_backoff_ms": 2000
      },
      "tier-1": {
        "max_retries": 3,
        "base_backoff_ms": 100,
        "backoff_multiplier": 2.0
      }
    }
  },
  "Frontend": {
    "claude_code": {
      "modelMappings": {
        "claude-3-5-sonnet-20241022": "anthropic,claude-3-5-sonnet-20241022",
        "claude-3-opus-20240229": "anthropic,claude-3-opus-20240229",
        "claude-3-5-haiku-20241022": "anthropic,claude-3-5-haiku-20241022",
        "glm-5": "zai,glm-5",
        "deepseek-reasoner": "deepseek,deepseek-reasoner"
      }
    }
  },
  "Presets": {
    "coding": {
      "route": "zai,glm-5",
      "max_tokens": 8192,
      "temperature": 0.2
    },
    "reasoning": {
      "route": "deepseek,deepseek-reasoner",
      "max_tokens": 16384,
      "temperature": 0.1
    },
    "documentation": {
      "route": "openrouter,minimax/minimax-m2.1",
      "max_tokens": 32768
    }
  },
  "PORT": 3456,
  "HOST": "127.0.0.1",
  "API_TIMEOUT_MS": 600000,
  "POOL_MAX_IDLE_PER_HOST": 100,
  "POOL_IDLE_TIMEOUT_MS": 60000
}
```

### 2.2 Model Mappings Explained

The `Frontend.claude_code.modelMappings` section maps Claude Code model names to CCR-Rust provider routes:

| Mapping Key | CCR-Rust Route | Description |
|-------------|----------------|-------------|
| `claude-3-5-sonnet-20241022` | `anthropic,claude-3-5-sonnet-20241022` | Default Claude 3.5 Sonnet |
| `claude-3-opus-20240229` | `anthropic,claude-3-opus-20240229` | Claude 3 Opus (powerful) |
| `claude-3-5-haiku-20241022` | `anthropic,claude-3-5-haiku-20241022` | Fast, cost-effective |
| `glm-5` | `zai,glm-5` | Z.AI GLM-5 via CCR-Rust |
| `deepseek-reasoner` | `deepseek,deepseek-reasoner` | DeepSeek reasoning model |

**Format:** `"provider,model"` where `provider` matches a provider name and `model` is in that provider's models list.

### 2.3 Start CCR-Rust

```bash
# Using the helper script
cd /path/to/AlphaHENG
./scripts/ccr-rust.sh start

# Or manually
ccr-rust start --config ~/.claude-code-router/config.json
```

Verify CCR-Rust is running:

```bash
curl http://127.0.0.1:3456/health
```

Expected response:
```json
{"status":"healthy","version":"1.0.0"}
```

### 2.4 Configure Claude Code Environment

Set the environment variables to point Claude Code to CCR-Rust:

```bash
# Point Claude Code to CCR-Rust proxy
export ANTHROPIC_BASE_URL="http://127.0.0.1:3456/v1"

# Your Anthropic API key (or any valid key for the upstream provider)
export ANTHROPIC_API_KEY="your-anthropic-api-key"

# Optional: Set default model
export CLAUDE_MODEL="claude-3-5-sonnet-20241022"
```

Add to your shell profile for persistence:

```bash
# ~/.zshrc or ~/.bashrc
export ANTHROPIC_BASE_URL="http://127.0.0.1:3456/v1"
export ANTHROPIC_API_KEY="your-anthropic-api-key"
```

---

## 3. Cache Control Settings

Claude Code supports prompt caching for improved performance and cost savings. CCR-Rust passes through cache control headers to compatible backends.

### 3.1 Anthropic Cache Control

When routing to Anthropic models, CCR-Rust supports the `cache_control` extension:

```json
{
  "Providers": [
    {
      "name": "anthropic",
      "api_base_url": "https://api.anthropic.com/v1/messages",
      "api_key": "${ANTHROPIC_API_KEY}",
      "models": ["claude-3-5-sonnet-20241022", "claude-3-opus-20240229"]
    }
  ]
}
```

### 3.2 Claude Code Cache Behavior

Claude Code automatically uses cache control for:
- **System prompts**: Cached automatically on first request
- **File contents**: Cached when files are read via tools
- **Conversation history**: Cached for multi-turn conversations

### 3.3 Verifying Cache Usage

Check CCR-Rust metrics to monitor cache performance:

```bash
# Check token usage including cache metrics
curl http://127.0.0.1:3456/v1/usage
```

Expected response:
```json
{
  "tier-0": {
    "input": 15000,
    "output": 5000,
    "cache_write": 10000,
    "cache_read": 8000
  }
}
```

### 3.4 Cache Configuration Tips

For optimal caching with Claude Code:

1. **Keep CCR-Rust running**: Cache tokens persist only while the connection is active
2. **Use connection pooling**: Configured by default with `POOL_MAX_IDLE_PER_HOST: 100`
3. **Monitor cache hit rates**: Use Prometheus metrics at `/metrics`

```bash
# Check cache-related metrics
curl http://127.0.0.1:3456/metrics | grep cache
```

---

## 4. Thinking Block Preferences

When using reasoning models (like DeepSeek Reasoner), Claude Code can display thinking blocks. CCR-Rust handles thinking content transformation automatically.

### 4.1 Enabling Thinking Blocks

Configure the `think` route in CCR-Rust for reasoning models:

```json
{
  "Router": {
    "think": "deepseek,deepseek-reasoner"
  }
}
```

### 4.2 Claude Code Display Preferences

Claude Code displays thinking blocks differently based on the model:

| Model | Thinking Display | Configuration |
|-------|-----------------|---------------|
| Claude 3 Opus | Native thinking | Automatic |
| DeepSeek Reasoner | Tagged blocks | `<think>...</think>` |
| GLM-5 | Via transformer | Hidden by default |

### 4.3 Controlling Thinking Output

Use CCR-Rust transformers to customize thinking block handling:

```json
{
  "Providers": [
    {
      "name": "deepseek",
      "api_base_url": "https://api.deepseek.com/chat/completions",
      "api_key": "${DEEPSEEK_API_KEY}",
      "models": ["deepseek-reasoner"],
      "transformer": {
        "use": ["deepseek", "thinktag", "openai_to_anthropic"]
      }
    }
  ]
}
```

**Transformer options:**
- `thinktag`: Wraps thinking content in `<think>` tags
- `thinkstrip`: Removes thinking content entirely

### 4.4 Environment Variable Control

Control thinking behavior via environment variables:

```bash
# Show thinking blocks (default for reasoning models)
export CLAUDE_SHOW_THINKING="true"

# Hide thinking blocks
export CLAUDE_SHOW_THINKING="false"
```

### 4.5 Verifying Thinking Block Handling

Test thinking block transformation:

```bash
curl -X POST http://127.0.0.1:3456/v1/messages \
  -H "Content-Type: application/json" \
  -H "x-anthropic-beta: output-128k-2025-02-19" \
  -d '{
    "model": "deepseek-reasoner",
    "messages": [{"role": "user", "content": "Explain the halting problem"}],
    "max_tokens": 4000
  }'
```

---

## Reasoning Provider Support

CCR-Rust now normalizes reasoning output from different providers into a single OpenAI-compatible field: `reasoning_content`.

### Unified Output Format

All reasoning-capable providers now return `reasoning_content` as a structured field in OpenAI responses. This keeps reasoning separate from normal assistant text content and enables reliable multi-turn tool use across providers.

### Provider Matrix

| Provider | Input Format | Output Format |
|----------|--------------|---------------|
| DeepSeek | `reasoning_content` (native) | `reasoning_content` (preserved) |
| Minimax M2.1 | `reasoning_details` | `reasoning_content` (mapped) |
| GLM-5 (Z.AI) | `<think>` tags | `reasoning_content` (extracted) |
| Kimi K2 | `◁think▷` tokens | `reasoning_content` (extracted) |

### Multi-Turn Tool Use

For multi-turn tool-calling conversations, pass `reasoning_content` back in assistant messages for providers that require reasoning continuity. DeepSeek reasoning models require this field on assistant turns involved in tool use.

If no reasoning text is available, send an empty string:

```json
{
  "role": "assistant",
  "content": "",
  "reasoning_content": "",
  "tool_calls": [
    {
      "id": "call_123",
      "type": "function",
      "function": {
        "name": "read_file",
        "arguments": "{\"path\":\"README.md\"}"
      }
    }
  ]
}
```

### Configuration (`config.alphaheng.json`)

Provider setup used by AlphaHENG (`contrib/ccr-rust/config.alphaheng.json`):

```json
{
  "Providers": [
    {
      "name": "zai",
      "api_base_url": "https://api.z.ai/api/inference/v1",
      "api_key": "${ZAI_API_KEY}",
      "models": ["glm-5"]
    },
    {
      "name": "deepseek",
      "api_base_url": "https://api.deepseek.com/v1",
      "api_key": "${DEEPSEEK_API_KEY}",
      "models": ["deepseek-reasoner"]
    },
    {
      "name": "minimax",
      "api_base_url": "https://api.minimax.io/v1",
      "api_key": "${MINIMAX_API_KEY}",
      "models": ["MiniMax-M2.1"]
    },
    {
      "name": "openrouter",
      "api_base_url": "https://openrouter.ai/api/v1",
      "api_key": "${OPENROUTER_API_KEY}",
      "models": ["openrouter/pony-alpha"]
    }
  ],
  "Router": {
    "default": "zai,glm-5",
    "think": "deepseek,deepseek-reasoner",
    "longContext": "minimax,MiniMax-M2.1"
  }
}
```

---

## 6. Troubleshooting

### 6.1 "Connection Refused" Error

**Symptom:**
```
Error: connect ECONNREFUSED 127.0.0.1:3456
```

**Solutions:**
1. Ensure CCR-Rust is running:
   ```bash
   ccr-rust status
   ```

2. Check the correct port is configured:
   ```bash
   lsof -i :3456
   ```

3. Start CCR-Rust if not running:
   ```bash
   ccr-rust start
   ```

### 6.2 "Invalid API Key" Error

**Symptom:**
```
Error: 401 Unauthorized - Invalid API key
```

**Solutions:**
1. Verify your API key is set correctly:
   ```bash
   echo $ANTHROPIC_API_KEY
   ```

2. Check CCR-Rust config has the correct provider API key:
   ```bash
   ccr-rust validate
   ```

3. Ensure environment variable substitution is working in config:
   ```json
   "api_key": "${ANTHROPIC_API_KEY}"
   ```

### 6.3 "Model Not Found" Error

**Symptom:**
```
Error: 404 - Model 'xxx' not found
```

**Solutions:**
1. Check model mapping in CCR-Rust config:
   ```json
   "Frontend": {
     "claude_code": {
       "modelMappings": {
         "your-model": "provider,actual-model-name"
       }
     }
   }
   ```

2. Verify the provider supports the requested model:
   ```bash
   curl http://127.0.0.1:3456/v1/models
   ```

3. Use the default model:
   ```bash
   claude --model claude-3-5-sonnet-20241022
   ```

### 6.4 High Latency or Timeouts

**Symptom:** Slow responses or timeout errors.

**Solutions:**
1. Check CCR-Rust latency metrics:
   ```bash
   curl http://127.0.0.1:3456/v1/latencies
   ```

2. Increase timeout in CCR-Rust config:
   ```json
   "API_TIMEOUT_MS": 600000
   ```

3. Check backend provider status:
   ```bash
   curl http://127.0.0.1:3456/metrics | grep ccr_failures
   ```

### 6.5 Cache Not Working

**Symptom:** No cache hit rate improvement, high token costs.

**Solutions:**
1. Verify cache metrics are being recorded:
   ```bash
   curl http://127.0.0.1:3456/v1/usage
   ```

2. Ensure using compatible model (Claude 3.5 Sonnet/Opus):
   ```bash
   claude --model claude-3-5-sonnet-20241022
   ```

3. Check CCR-Rust is keeping connections alive:
   ```bash
   curl http://127.0.0.1:3456/metrics | grep pool
   ```

### 6.6 Thinking Blocks Not Displaying

**Symptom:** Reasoning models not showing thinking content.

**Solutions:**
1. Verify think transformer is configured:
   ```json
   "transformer": {
     "use": ["deepseek", "thinktag", "openai_to_anthropic"]
   }
   ```

2. Check Claude Code environment variable:
   ```bash
   echo $CLAUDE_SHOW_THINKING
   ```

3. Test with a reasoning prompt:
   ```bash
   claude --model deepseek-reasoner
   ```

### 6.7 Tool Use Failures

**Symptom:** Claude Code cannot use tools (file operations, bash commands).

**Solutions:**
1. Ensure using native Anthropic route for full tool support:
   ```bash
   claude --model claude-3-5-sonnet-20241022
   ```

2. Check transformer supports tool translation:
   ```json
   "transformer": {
     "use": ["openai_to_anthropic"]
   }
   ```

3. Verify CCR-Rust metrics show tool usage:
   ```bash
   curl http://127.0.0.1:3456/metrics | grep tool
   ```

### 6.8 Debug Logging

Enable debug output for troubleshooting:

**CCR-Rust debug logs:**
```bash
RUST_LOG=ccr_rust=debug ccr-rust start
```

**Claude Code debug output:**
```bash
DEBUG=* claude
```

**Verbose Claude Code output:**
```bash
claude --verbose
```

### 6.9 Verify End-to-End Flow

Test the complete flow manually:

```bash
# 1. Test CCR-Rust health
curl http://127.0.0.1:3456/health

# 2. Test messages endpoint
curl -X POST http://127.0.0.1:3456/v1/messages \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $ANTHROPIC_API_KEY" \
  -H "anthropic-version: 2023-06-01" \
  -d '{
    "model": "claude-3-5-sonnet-20241022",
    "messages": [{"role": "user", "content": "Hello"}],
    "max_tokens": 100
  }'

# 3. Test via Claude Code
claude exec --model claude-3-5-sonnet-20241022 "Say hello"
```

---

## Advanced Configuration

### Using Presets with Claude Code

CCR-Rust supports preset routes that Claude Code can use:

```bash
# Configure preset in CCR-Rust config
"Presets": {
  "coding": {
    "route": "zai,glm-5",
    "max_tokens": 8192,
    "temperature": 0.2
  }
}
```

Access via direct URL:
```bash
# Use preset endpoint directly
curl http://127.0.0.1:3456/preset/coding/v1/messages \
  -H "Content-Type: application/json" \
  -d '{"messages": [...]}'
```

### Multi-Provider Routing

CCR-Rust automatically routes based on model availability and latency. Claude Code requests will be routed to the best available backend:

```json
{
  "Router": {
    "default": "anthropic,claude-3-5-sonnet-20241022",
    "think": "deepseek,deepseek-reasoner",
    "longContext": "openrouter,minimax/minimax-m2.1"
  }
}
```

---

## References

- [CCR-Rust Configuration](./configuration.md) - Full configuration reference
- [CCR-Rust CLI](./cli.md) - CLI commands and options
- [Claude Code API Research](./claude_code_api_research.md) - Anthropic API format details
- [Troubleshooting](./troubleshooting.md) - General CCR-Rust troubleshooting
- [Anthropic Claude Documentation](https://docs.anthropic.com) - Official Claude docs
- [Claude Code Documentation](https://docs.anthropic.com/en/docs/claude-code/overview) - Official Claude Code docs
