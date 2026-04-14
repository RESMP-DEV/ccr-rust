# Token Optimization Guide

CCR-Rust provides five mechanisms for reducing token usage across the multi-tier proxy. Each targets a different part of the request/response lifecycle. They can be combined for cumulative savings.

## Overview

| Feature | Direction | What it compresses | Typical savings |
|---------|-----------|-------------------|-----------------|
| [Output Compression](#output-compression) | Response | Verbose tool output (cargo, git, grep, npm, test runners) | 30–70% on build logs |
| [Tool Catalog Compression](#tool-catalog-compression) | Request | Tool definitions in `tools` array | 20–60% on tool schema tokens |
| [MCP Catalog Compression](#mcp-catalog-compression) | Request | MCP tool catalogs aggregated from backends | 30–50% on MCP tool definitions |
| [Think Tag Stripping](#think-tag-stripping) | Response | `<think>`, `<thinking>`, `<reasoning>` blocks | 100% of reasoning overhead |
| [Prompt Caching](#prompt-caching) | Request | Repeated context via Anthropic `cache_control` | Up to 90% on cache hits |

### Aggregate savings by configuration level

These are rough estimates for a typical agentic coding session (20+ tools, iterative build/test loops, multi-turn conversations):

| Level | What's enabled | Estimated aggregate savings |
|-------|---------------|----------------------------|
| **None** | No transformers | 0% — every token passes through verbatim |
| **Conservative** | `output_compress` only | ~15–25% — cuts build/test log bloat, zero risk |
| **Recommended** (default) | `output_compress` + `toolcompress` (medium) + `thinktag` | ~30–45% — compresses both directions, strips reasoning overhead |
| **Aggressive** | All of the above + `toolcompress` (high) | ~45–65% — maximum compression, may degrade tool selection on weaker models |

The savings compound: `toolcompress` reduces input tokens per request (20+ tools → ~40% smaller tool schema), `output_compress` reduces output tokens per response (build logs → 30–70% smaller), and `thinktag` removes reasoning blocks that can be 500+ tokens per turn. Server-side caching (automatic on all providers) further reduces effective costs without any client-side configuration.

## Defaults

The starter config (`config.example.json`) demonstrates these transformers, but usage is intentionally provider-specific. A practical starting point is the **Recommended** level:

- enable `toolcompress` (medium) on providers with large tool catalogs or tight token budgets,
- enable `output_compress` for agent workloads that return verbose tool output,
- enable `thinktag` on reasoning-capable models when you want to discard visible thinking output,
- add provider-specific transformers such as `kimi` only where required.

Some deployments choose to disable compression on flat-rate or fidelity-sensitive tiers; that is an operator preference, not a CCR-Rust requirement.

| Transformer | Suggested default | Applies to | Notes |
|------------|-------------------|------------|-------|
| `toolcompress` (medium) | **Recommended** | Providers with large tool catalogs | Truncates tool descriptions to 200 chars, strips property descriptions |
| `output_compress` | **Recommended** | Agent workloads with verbose tool output | Pattern-based compression of build logs, test output, grep results |
| `thinktag` | **Optional** | Reasoning-capable models | Strips standard `<think>` blocks from reasoning model output |
| `kimi` | **Optional** | Kimi only | Extracts Unicode think tokens (◁think▷/◁/think▷) into `reasoning_content` |
| `enhancetool` | **Optional** | Anthropic-compatible tool workflows | Adds `cache_control` annotations where the upstream may honor them |

### Provider prompt caching landscape

Each provider implements caching differently. None of the CCR-Rust providers use Anthropic-style `cache_control` annotations — they all have **server-side automatic caching** that requires no client-side opt-in.

| Provider | Protocol | Caching mechanism | `enhancetool` effect |
|----------|----------|-------------------|---------------------|
| Kimi | Anthropic (`/coding/v1`) | **Server-side automatic.** Uses `prompt_cache_key` param (session/task ID) for cache affinity. Returns `cached_tokens` in usage response. | No-op — Kimi's caching is server-side; `cache_control` annotations are ignored. |
| Minimax | Anthropic (`/anthropic/v1`) | **Server-side automatic.** Returns `cache_creation_input_tokens` and `cache_read_input_tokens` in usage. `cache_control` is NOT listed as a supported parameter. | No-op — caching happens automatically server-side. |
| Z.AI/GLM | OpenAI (via `anthropic` transformer) | **Server-side implicit.** "Preserved Thinking" (`clear_thinking: false`) increases cache hit rates on the Coding Plan endpoint. No explicit caching API. | No-op — `cache_control` is lost in Anthropic→OpenAI conversion. |
| Gemini | OpenAI-compatible | **Implicit caching** auto-enabled on Gemini 2.5+ (min 1024 tokens). Also supports Explicit caching via separate `CachedContent` API (not `cache_control`). Returns hits in `usage_metadata`. | No-op — uses `cached_content` references, not `cache_control`. |
| DigitalOcean | OpenAI | **Server-side automatic.** Returns `prompt_tokens_details.cached_tokens` in response. No request-side caching API. | No-op — OpenAI protocol, no `cache_control` support. |

**Key takeaway:** Anthropic-style `cache_control: {type: "ephemeral"}` is only meaningful when talking directly to Anthropic's API (Claude, tier 0), which doesn't route through CCR-Rust. For all CCR-Rust providers, caching happens automatically server-side — `enhancetool` adds harmless but useless annotations. It is excluded from the default config template to avoid implying client-controlled caching where there is none.

### Maximizing server-side cache hits

Although you can't control caching via `cache_control`, you can improve hit rates:

- **Kimi:** Pass a stable `prompt_cache_key` (session ID or task ID) for cache affinity across multi-turn conversations.
- **Z.AI/GLM:** Use `clear_thinking: false` (Preserved Thinking) on the Coding Plan endpoint to keep reasoning context intact and increase cache hits.
- **Gemini:** Place large, stable content at the beginning of prompts. Send similar-prefix requests close together in time.
- **Minimax/DigitalOcean:** Caching is fully automatic — no tuning knobs available.

### Upgrading existing installs

If your current `~/.claude-code-router/config.json` predates these transformers, either copy a fresh `config.example.json` into place and re-apply your API keys/routes, or add them manually:

```json
"transformer": {
  "use": [
    "anthropic",
    ["toolcompress", {"level": "medium"}],
    "output_compress"
  ]
}
```

**Kimi provider note:** If you add a `kimi` provider, consider the `kimi`
transformer (for Unicode think-token extraction), `enhancetool`, and any
`extra_headers` you need for downstream client identification. Add them
manually in your provider config.

## Output Compression

**Transformer name:** `output_compress`

Compresses verbose `tool_result` text in responses before they're returned to the client. Targets repetitive build output, log spam, and progress indicators that consume tokens without adding useful context.

### How it works

1. Walks response `content` arrays for `tool_result` blocks containing text
2. Applies universal cleanup: strips ANSI escape codes, collapses 3+ blank lines, removes carriage-return progress bars
3. Applies command-specific pattern compression (first match wins):
   - **git** — Collapses diff stat summaries, repeated hunk headers
   - **cargo** — Strips `Compiling`/`Downloading` lines, deduplicates warnings
   - **test runners** — Condenses pass/fail line sequences
   - **npm** — Removes install progress, deduplicates audit lines
   - **grep** — Deduplicates matching patterns
4. Skips inputs shorter than 200 characters
5. Returns original unchanged if compression ratio exceeds 0.85 (not worth the info loss)

### Configuration

```json
{
  "transformer": {
    "use": ["output_compress"]
  }
}
```

No options — the transformer is applied unconditionally to all tool results when enabled.

### When to use

- Agents running build commands (`cargo build`, `npm install`, `cmake`)
- Agents doing file searches (`grep`, `rg`, `find`)
- Agents running test suites with verbose output
- Any workload where tool results are long but mostly repetitive

## Tool Catalog Compression

**Transformer name:** `toolcompress`

Compresses tool definitions in the `tools` array of requests. When agents have 20+ tools (common with MCP), tool schemas can consume 5,000–15,000 tokens per request.

### Compression levels

| Level | Behavior | Token impact |
|-------|----------|-------------|
| `low` | No compression (passthrough) | 0% savings |
| `medium` | Truncates tool descriptions to 200 chars, removes property descriptions from `input_schema` | ~40% reduction |
| `high` | Removes tool descriptions entirely, strips properties down to keys + types only | ~60% reduction |

### Configuration

```json
{
  "transformer": {
    "use": [["toolcompress", {"level": "medium"}]]
  }
}
```

### Trade-offs

- **Medium:** Safe for most models. Retains tool names and truncated descriptions.
- **High:** May degrade tool selection accuracy on weaker models. Use when token budget is tight and models are strong enough to infer tool usage from names alone.

## MCP Catalog Compression

**CLI flag:** `--level`

When CCR-Rust aggregates tool catalogs from multiple MCP backends, the combined catalog can be large. Compression reduces the catalog before serving it to clients.

### Compression levels

| Level | Behavior |
|-------|----------|
| `none` | Removes descriptions AND schemas (keys only) |
| `minimal` / `high` / `aggressive` | Removes descriptions, keeps schemas |
| `medium` / `light` | Normalizes whitespace in descriptions, truncates to 256 chars |
| (default) | Full descriptions and schemas |

### Usage

```bash
ccr-rust mcp --level medium \
  --wrap "npx @anthropic/mcp-server-filesystem" \
  --wrap "npx @zilliz/claude-context-mcp@latest"
```

### Recommendation

Use `medium` for production. The `none` level strips too much context and can confuse models about tool parameter requirements.

## Think Tag Stripping

**Transformer name:** `thinktag`

Strips reasoning blocks from response text content. Models like DeepSeek-R1, Kimi, and others emit `<think>...</think>` (or `<thinking>`, `<reasoning>`) tags containing chain-of-thought. These blocks are useful for debugging but consume output tokens on every response.

### What gets stripped

```
<think>Let me analyze this step by step...</think>       → removed
<thinking>The user wants to...</thinking>                 → removed
<reasoning>First, I'll check the file...</reasoning>      → removed
```

The regex: `(?s)<think>.*?</think>|<thinking>.*?</thinking>|<reasoning>.*?</reasoning>`

### Configuration

```json
{
  "transformer": {
    "use": ["thinktag"]
  }
}
```

### When to use

- When routing through reasoning models (DeepSeek-R1, Kimi K2.5, QwQ)
- When you want reasoning quality but don't need the thinking output
- When output token budget is limited

### Related: Reasoning Transformer

The `reasoning` transformer (separate from `thinktag`) extracts the `reasoning_content` field from DeepSeek-style responses and formats it as a proper `thinking` content block. Use `reasoning` when you want to _preserve_ thinking; use `thinktag` when you want to _discard_ it.

## Prompt Caching

**Transformer name:** `enhancetool`

Enables Anthropic's prompt caching by annotating tool use blocks with `cache_control: { type: "ephemeral" }` metadata. When Anthropic sees this annotation, it caches the associated content and serves it from cache on subsequent requests in the same session.

### How it works

1. The `enhancetool` transformer adds `cache_control` metadata to all `tool_use` blocks in responses
2. On the next turn, when the client sends back the conversation history, Anthropic recognizes cached blocks
3. Cached tokens are served at ~10% of normal cost (reported as `cache_read_input_tokens`)

### Configuration

The `enhancetool` transformer is a built-in that runs on response processing:

```json
{
  "transformer": {
    "use": ["enhancetool"]
  }
}
```

### Verifying cache hits

Check the `/v1/usage` endpoint for cache metrics:

```bash
curl -s http://127.0.0.1:3456/v1/usage | jq '.tiers[].cache_read_tokens'
```

Or monitor via Prometheus metrics:

- `ccr_cache_read_tokens_total{tier}` — tokens served from cache
- `ccr_cache_creation_tokens_total{tier}` — tokens used to populate cache

A healthy caching setup shows `cache_read_tokens` growing faster than `cache_creation_tokens` over time.

## Combining Features

All five features operate at different points in the pipeline and can be combined freely:

```json
{
  "Providers": [
    {
      "name": "zai",
      "api_base_url": "https://api.z.ai/api/coding/paas/v4",
      "api_key": "${ZAI_API_KEY}",
      "models": ["glm-5.1"],
      "transformer": {
        "use": [
          "anthropic",
          ["toolcompress", {"level": "medium"}],
          "output_compress",
          "thinktag"
        ]
      }
    }
  ]
}
```

**Ordering:** Transformers execute in declaration order. For token optimization:

1. Protocol transformers first (`anthropic`, `deepseek`, etc.)
2. Request compression (`toolcompress`) — reduces tokens sent upstream
3. Response compression (`output_compress`, `thinktag`) — reduces tokens returned to client

## Monitoring Token Savings

### HTTP Endpoints

| Endpoint | What it shows |
|----------|---------------|
| `GET /v1/usage` | Per-tier aggregate: input tokens, output tokens, cache read/creation tokens |
| `GET /v1/token-drift` | Accuracy of local token estimates vs upstream-reported counts |
| `GET /v1/token-audit` | Ring buffer (1024 entries) of pre-request token breakdowns by component (messages, system, tools) |

### Prometheus Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `ccr_input_tokens_total{tier}` | counter | Total input tokens consumed per tier |
| `ccr_output_tokens_total{tier}` | counter | Total output tokens generated per tier |
| `ccr_cache_read_tokens_total{tier}` | counter | Tokens served from prompt cache |
| `ccr_cache_creation_tokens_total{tier}` | counter | Tokens used to create cache entries |
| `ccr_pre_request_tokens_total{tier,component}` | counter | Estimated input tokens before dispatch (component: messages, system, tools) |
| `ccr_pre_request_tokens{tier}` | histogram | Distribution of pre-request token counts |
| `ccr_token_drift_absolute{tier}` | gauge | Local estimate minus upstream reported |
| `ccr_token_drift_pct{tier}` | gauge | Percentage drift in token accounting |
| `ccr_token_drift_alerts_total{tier,severity}` | counter | Drift threshold violations |

### TUI Dashboard

```bash
ccr-rust dashboard
```

The dashboard shows live token throughput, cache hit rates, and per-tier token drift.

### Measuring Savings

To quantify token reduction from compression:

1. **Before:** Run a workload without `toolcompress` and `output_compress`. Record `ccr_input_tokens_total` and `ccr_output_tokens_total`.
2. **After:** Enable the transformers and run the same workload. Compare totals.
3. **Cache ROI:** Compare `ccr_cache_read_tokens_total` vs `ccr_cache_creation_tokens_total`. Read tokens should dominate after warmup.

The `pre_request_tokens` component breakdown (messages vs system vs tools) shows where tokens are being spent, helping you target which compression features matter most for your workload.
