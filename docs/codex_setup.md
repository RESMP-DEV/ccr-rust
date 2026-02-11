# Codex CLI Setup Guide for CCR-Rust

This guide covers setting up OpenAI Codex CLI to work with CCR-Rust as a proxy router, enabling intelligent routing to multiple LLM backends including Z.AI GLM, DeepSeek Reasoner, MiniMax M2.1, and more.

> **Note:** Codex CLI is the **primary recommended frontend** for CCR-Rust. While Claude Code is also supported, Codex provides better compatibility with CCR-Rust's multi-provider routing and reasoning normalization features.

## Table of Contents

1. [Installing Codex CLI](#1-installing-codex-cli)
2. [Configuring CCR-Rust for Codex](#2-configuring-ccr-rust-for-codex)
3. [Setting Environment Variables](#3-setting-environment-variables)
4. [Running Codex with CCR-Rust Proxy](#4-running-codex-with-ccr-rust-proxy)
5. [Reasoning Provider Support](#5-reasoning-provider-support)
6. [OpenRouter Attribution](#6-openrouter-attribution)
7. [Troubleshooting Common Issues](#7-troubleshooting-common-issues)

---

## 1. Installing Codex CLI

Install the official OpenAI Codex CLI globally using npm:

```bash
npm install -g @openai/codex
```

Verify the installation:

```bash
codex --version
```

Expected output:
```
codex-cli/1.0.0 (or newer)
```

### System Requirements

- Node.js 18 or higher
- npm 8 or higher
- An OpenAI API key (or compatible endpoint via CCR-Rust)

### Alternative: Local Installation

If you prefer not to install globally:

```bash
# Using npx (no installation required)
npx @openai/codex --version
```

---

## 2. Configuring CCR-Rust for Codex

CCR-Rust provides an OpenAI-compatible endpoint at `/v1/chat/completions` that Codex CLI can use. You need to configure model mappings in your CCR-Rust config file.

### 2.1 Create/Edit Configuration File

Edit your CCR-Rust configuration file (default: `~/.claude-code-router/config.json`):

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
      "models": ["deepseek-reasoner", "deepseek-chat"]
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
    "longContext": "minimax,MiniMax-M2.1",
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
        "backoff_multiplier": 2.0,
        "max_backoff_ms": 5000
      }
    }
  },
  "Presets": {
    "coding": {
      "route": "zai,glm-5",
      "temperature": 0.7
    },
    "reasoning": {
      "route": "deepseek,deepseek-reasoner"
    },
    "documentation": {
      "route": "minimax,MiniMax-M2.1"
    }
  },
  "PORT": 3456,
  "HOST": "127.0.0.1",
  "API_TIMEOUT_MS": 120000,
  "SSE_BUFFER_SIZE": 1024,
  "POOL_MAX_IDLE_PER_HOST": 100,
  "POOL_IDLE_TIMEOUT_MS": 30000
}
```

Use separate variables to avoid confusion:
- `OPENAI_API_KEY`: Codex CLI client token for calls to CCR-Rust (any non-empty string).
- `ZAI_API_KEY`, `DEEPSEEK_API_KEY`, etc.: Actual upstream keys used by CCR-Rust in `Providers[].api_key`.

### 2.2 Provider Routing Explained

The `Router` section configures automatic tier-based routing:

| Route | CCR-Rust Route | Description |
|-------|----------------|-------------|
| `default` | `zai,glm-5` | Primary tier—requests go here first |
| `think` | `deepseek,deepseek-reasoner` | Used for reasoning-heavy tasks |
| `longContext` | `minimax,MiniMax-M2.1` | Used when token count exceeds threshold |

**Format:** `"provider,model"` where `provider` matches a provider name and `model` is in that provider's models list.

### 2.3 Presets Explained

Presets provide named routing configurations for different task types:

| Preset | Route | Use Case |
|--------|-------|----------|
| `coding` | Z.AI GLM-5 | Fast code generation (P0/P1 tasks) |
| `reasoning` | DeepSeek Reasoner | Complex analysis requiring CoT |
| `documentation` | MiniMax M2.1 | Long-form content generation |

### 2.4 Start CCR-Rust

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

---

## 3. Setting Environment Variables

Codex CLI uses environment variables to configure the API endpoint and authentication.

### 3.1 Required Variables

```bash
# Point Codex to CCR-Rust proxy instead of OpenAI directly
export OPENAI_BASE_URL="http://127.0.0.1:3456/v1"

# Any non-empty string works (for example: dummy-key, test, abc123)
# CCR-Rust ignores this incoming token value for upstream auth
export OPENAI_API_KEY="any-non-empty-string"

# Optional: Default model for Codex
export CODEX_MODEL="gpt-4o"
```

### 3.2 Provider-Specific API Keys

If your CCR-Rust config routes to multiple providers, set keys for each:

```bash
# Add to your ~/.bashrc, ~/.zshrc, or ~/.env file
export OPENAI_PROVIDER_API_KEY="sk-openai-..."
export DEEPSEEK_API_KEY="sk-deepseek-..."
export OPENROUTER_API_KEY="sk-or-v1-..."
export ZAI_API_KEY="your-zai-key"
```

### 3.3 Persistent Configuration

Add to your shell profile for persistence:

```bash
# ~/.zshrc or ~/.bashrc
export OPENAI_BASE_URL="http://127.0.0.1:3456/v1"
export OPENAI_API_KEY="any-non-empty-string"
```

Or use a `.env` file in your project:

```bash
# Load from .env file
set -a && source .env && set +a
```

---

## 4. Running Codex with CCR-Rust Proxy

Once configured, run Codex normally. All requests will be routed through CCR-Rust.

### 4.1 Interactive Mode

```bash
# Start interactive session
codex

# With specific model (routes via CCR-Rust mapping)
codex --model gpt-4o

# Use reasoning model via CCR-Rust routing
codex --model o3-mini
```

### 4.2 Non-Interactive Mode

```bash
# Single command execution
codex exec "Explain this codebase structure"

# With JSON output
codex exec --json "List all Python files"

# Using a specific model via CCR-Rust
codex exec --model deepseek-chat "Review this code"
```

### 4.3 Full Mode (Agent Mode)

```bash
# Full agent mode with tool access
codex exec --full "Implement a REST API for user management"
```

### 4.4 Verify Routing

Check CCR-Rust metrics to confirm Codex requests are being routed:

```bash
# Check latency metrics
curl http://127.0.0.1:3456/v1/latencies

# Check Prometheus metrics
curl http://127.0.0.1:3456/metrics | grep ccr_requests_total
```

---

## 5. Reasoning Provider Support

CCR-Rust normalizes reasoning output from different providers into a unified OpenAI-compatible field: `reasoning_content`. This keeps reasoning separate from normal assistant text and enables reliable multi-turn tool use.

### 5.1 Unified Output Format

All reasoning-capable providers return `reasoning_content` as a structured field:

| Provider | Input Format | Output Format |
|----------|--------------|---------------|
| DeepSeek | `reasoning_content` (native) | `reasoning_content` (preserved) |
| Minimax M2.1 | `reasoning_details` | `reasoning_content` (mapped) |
| GLM-5 (Z.AI) | `<think>` tags | `reasoning_content` (extracted) |
| Kimi K2 | `◁think▷` tokens | `reasoning_content` (extracted) |

### 5.2 Multi-Turn Tool Use

For multi-turn tool-calling conversations, pass `reasoning_content` back in assistant messages. DeepSeek Reasoner **requires** this field on assistant turns involved in tool use.

Example assistant message with tool call:

```json
{
  "role": "assistant",
  "content": "",
  "reasoning_content": "Let me analyze this step by step...",
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

If no reasoning is available, send an empty string:

```json
{
  "role": "assistant",
  "content": "Here's the file content.",
  "reasoning_content": ""
}
```

### 5.3 Verifying Reasoning Output

Test reasoning normalization:

```bash
curl -X POST http://127.0.0.1:3456/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer test" \
  -d '{
    "model": "deepseek-reasoner",
    "messages": [{"role": "user", "content": "What is 15 * 23?"}],
    "max_tokens": 1000
  }'
```

Expected response will include `reasoning_content` in the message.

---

## 6. OpenRouter Attribution

When routing through OpenRouter, CCR-Rust automatically includes attribution headers:

- `HTTP-Referer`: `https://github.com/RESMP-DEV/ccr-rust`
- `X-Title`: `ccr-rust`

This enables proper usage tracking and token attribution on the OpenRouter platform. No configuration required—headers are added automatically when the provider is detected as OpenRouter.

---

## 7. Troubleshooting Common Issues

### 7.1 "Connection Refused" Error

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

### 7.2 "Invalid API Key" Error

**Symptom:**
```
Error: 401 Unauthorized - Invalid API key
```

**Solutions:**
1. Verify the Codex client token is set (any non-empty string):
   ```bash
   echo $OPENAI_API_KEY
   ```

2. Check CCR-Rust config has the correct provider API key:
   ```bash
   ccr-rust validate
   ```

3. Ensure environment variable substitution is working in config:
   ```json
   "api_key": "${OPENAI_PROVIDER_API_KEY}"
   ```

### 7.3 "Model Not Found" Error

**Symptom:**
```
Error: 404 - Model 'xxx' not found
```

**Solutions:**
1. Check model mapping in CCR-Rust config:
   ```json
   "Frontend": {
     "codex": {
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
   codex --model gpt-4o
   ```

### 7.4 High Latency or Timeouts

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

### 7.5 Tool Calls Not Working

**Symptom:** Codex doesn't execute commands or file operations.

**Solutions:**
1. Ensure you're using `--full` mode:
   ```bash
   codex exec --full "Run the tests"
   ```

2. Check CCR-Rust supports tool transformation:
   ```bash
   # Check transformer registry
   curl http://127.0.0.1:3456/metrics | grep transformer
   ```

### 7.6 Debug Logging

Enable debug output for troubleshooting:

**CCR-Rust debug logs:**
```bash
RUST_LOG=ccr_rust=debug ccr-rust start
```

**Codex debug output:**
```bash
DEBUG=* codex exec "Test command"
```

### 7.7 Verify End-to-End Flow

Test the complete flow manually:

```bash
# 1. Test CCR-Rust health
curl http://127.0.0.1:3456/health

# 2. Test chat completions endpoint
# OPENAI_API_KEY can be any non-empty string for CCR-Rust
curl -X POST http://127.0.0.1:3456/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  -d '{
    "model": "gpt-4o",
    "messages": [{"role": "user", "content": "Hello"}],
    "max_tokens": 50
  }'

# 3. Test via Codex
codex exec --model gpt-4o "Say hello"
```

---

## Advanced Configuration

### Using Presets with Codex

CCR-Rust supports preset routes that Codex can use:

```bash
# Configure preset in CCR-Rust config
"Presets": {
  "coding": {
    "route": "openai,gpt-4o",
    "max_tokens": 4096,
    "temperature": 0.2
  }
}
```

Access via direct URL:
```bash
# Use preset endpoint directly
curl http://127.0.0.1:3456/preset/coding/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"messages": [...]}'
```

### Multi-Provider Routing

CCR-Rust automatically routes based on model availability and latency. Codex requests will be routed to the best available backend:

```json
{
  "Router": {
    "default": "openai,gpt-4o",
    "think": "deepseek,deepseek-reasoner",
    "background": "openrouter,minimax/minimax-m2.1"
  }
}
```

---

## References

- [CCR-Rust Configuration](./configuration.md) - Full configuration reference
- [CCR-Rust CLI](./cli.md) - CLI commands and options
- [Codex API Research](./codex_api_research.md) - OpenAI API format details
- [Troubleshooting](./troubleshooting.md) - General CCR-Rust troubleshooting
- [OpenAI Codex Documentation](https://developers.openai.com/codex) - Official Codex docs
