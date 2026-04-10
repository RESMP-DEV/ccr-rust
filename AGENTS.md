# CCR-Rust Agent Instructions

> `CLAUDE.md` is a symlink to this file. Both Claude Code and Codex read it automatically.

## Set Up from Scratch

Prerequisites: Rust toolchain (`rustup`), at least one LLM API key.

```bash
# 1. Build
cargo build --release
cargo install --path . --force

# 2. Create config directory and install the example config
mkdir -p ~/.claude-code-router
cp config.example.json ~/.claude-code-router/config.json

# 3. Add your API keys (edit the file or create a .env)
cat > ~/.claude-code-router/.env << 'EOF'
DEEPSEEK_API_KEY=sk-your-deepseek-key
MINIMAX_API_KEY=your-minimax-key
OPENROUTER_API_KEY=sk-or-your-openrouter-key
EOF

# 4. Start the router
ccr-rust start

# 5. Point Claude Code at CCR
export ANTHROPIC_BASE_URL=http://127.0.0.1:3456
claude
```

The config uses `${ENV_VAR}` placeholders. CCR-Rust loads `.env` files from the current directory and `~/.claude-code-router/.env` automatically.

**Verify it works:**

```bash
ccr-rust status     # Health check
ccr-rust validate   # Config validation
ccr-rust dashboard  # Live TUI dashboard
```

## What This Is

CCR-Rust is a multi-protocol AI proxy (~20K lines of Rust) that routes requests from Claude Code, Codex, and OpenAI-compatible clients through multiple LLM providers with automatic failover. It handles protocol translation, request/response transformation, MCP tool aggregation, token accounting, and streaming.

## Build & Test

```bash
cargo check          # Type check (fast)
cargo clippy         # Lint
cargo fmt            # Format
cargo test           # Run all tests
cargo build --release  # Release build (LTO enabled, slow)
```

After any code change, sync the CLI binary:

```bash
cargo install --path . --force
```

This keeps `~/.cargo/bin/ccr-rust` (used by `ccr-rust dashboard`, etc.) in sync with `target/release/ccr-rust` (used by `ccr-rust start`).

## Architecture

### Request Flow

```
Client Request
  -> Frontend detection (Claude Code or Codex)
  -> Parse into InternalRequest
  -> Build transformer chain from provider config
  -> Apply request transformers
  -> Route to provider (OpenAI or Anthropic protocol)
  -> On failure: record EWMA penalty, try next tier
  -> Apply response transformers
  -> Serialize to client's format
  -> Return response
```

### Key Files

| File                               | Purpose                                                                    |
| ---------------------------------- | -------------------------------------------------------------------------- |
| `src/main.rs`                      | CLI entry point, 8 subcommands (clap)                                      |
| `src/config.rs`                    | Config parsing, `Provider`, `RouterConfig`, `ProviderProtocol` enum        |
| `src/router/mod.rs`                | HTTP handlers (`handle_messages`, `handle_preset_messages`, `list_models`) |
| `src/router/types.rs`              | `AnthropicRequest`, `AppState`, `TryRequestArgs`, shared types             |
| `src/router/dispatch.rs`           | `try_request`, protocol dispatch, header building                          |
| `src/router/streaming.rs`          | SSE streaming + pseudo-streaming response translation                      |
| `src/router/openai_compat.rs`      | `/v1/chat/completions` handler                                             |
| `src/router/responses_api.rs`      | `/v1/responses` handler                                                    |
| `src/router/translate_request.rs`  | Anthropic→OpenAI request translation                                       |
| `src/router/translate_response.rs` | OpenAI→Anthropic response translation                                      |
| `src/routing.rs`                   | EWMA latency tracker per tier                                              |
| `src/transformer.rs`               | `Transformer` trait, common transformer impls                              |
| `src/transform/registry.rs`        | `TransformerRegistry` — maps names to transformer instances                |
| `src/frontend/mod.rs`              | `Frontend` trait, `InternalRequest`/`InternalResponse`                     |
| `src/frontend/codex.rs`            | OpenAI format parsing/serialization                                        |
| `src/frontend/claude_code.rs`      | Anthropic format parsing/serialization                                     |
| `src/metrics/mod.rs`               | Prometheus metric definitions, recording functions                         |
| `src/metrics/handlers.rs`          | HTTP handler endpoints (`/metrics`, `/usage`, `/latencies`)                |
| `src/metrics/persistence.rs`       | Redis persistence for metrics (save/restore)                               |
| `src/mcp/server.rs`                | MCP JSON-RPC server                                                        |
| `src/mcp/catalog.rs`               | Tool catalog aggregation and compression                                   |
| `src/sse.rs`                       | SSE frame parser/serializer                                                |
| `src/debug_capture.rs`             | Request/response capture to disk                                           |
| `src/ratelimit.rs`                 | Rate limit state, exponential backoff                                      |

### Protocols

The `ProviderProtocol` enum (`config.rs`) determines how requests are sent upstream:

- **`Openai`** (default) — POST to `{api_base_url}/chat/completions`. Uses `Authorization: Bearer {api_key}`. Most providers use this (including Gemini via OpenAI-compatible API).
- **`Anthropic`** — POST to `{api_base_url}/messages`. Uses `x-api-key` header. For Kimi, MiniMax when using native Anthropic protocol.

### Transformer System

Transformers implement the `Transformer` trait (`transformer.rs`):

```rust
pub trait Transformer: Send + Sync {
    fn transform_request(&self, request: Value) -> Result<Value>;
    fn transform_response(&self, response: Value) -> Result<Value>;
    fn name(&self) -> &str;
}
```

Registered in **both** `TransformerRegistry::new()` locations:

- `transformer/mod.rs` — the runtime registry used by `AppState`
- `transform/registry.rs` — the factory-based registry

To add a new transformer:

1. Create `src/transform/your_provider.rs` implementing `Transformer`
2. Add `pub mod your_provider;` to `src/transform/mod.rs`
3. Register in **both** `transformer/mod.rs::TransformerRegistry::new()` and `transform/registry.rs::TransformerRegistry::new()`
4. Reference in config: `"transformer": { "use": ["your_provider"] }`

Existing transformers: anthropic, anthropic-to-openai, deepseek, openai-to-anthropic, minimax, openrouter, tooluse, identity, reasoning, enhancetool, thinktag, glm, kimi.

### Frontend System

Frontends normalize between client formats. The `Frontend` trait (`frontend/mod.rs`) handles:

- Request parsing into `InternalRequest`
- Response serialization from `InternalResponse`
- Stream chunk formatting

Detection logic is in `frontend/detection.rs` — checks headers and body shape.

## Adding a New Provider

**If the provider uses standard OpenAI or Anthropic API format**: config-only, no code changes.

```json
{
  "name": "new_provider",
  "api_base_url": "https://api.example.com/v1",
  "api_key": "${NEW_PROVIDER_API_KEY}",
  "models": ["model-name"],
  "tier_name": "ccr-new"
}
```

Set `"protocol": "anthropic"` if the provider uses Anthropic's Messages API natively. Otherwise it defaults to OpenAI.

**If the provider needs request/response normalization**: add a transformer (see Transformer System above).

**If the provider uses a completely new protocol**: add a variant to `ProviderProtocol` in `config.rs`, add a handler function in `router/dispatch.rs`, and add a match arm in the protocol dispatch logic.

## Adding a New CLI Command

1. Add a variant to the clap `Commands` enum in `main.rs`
2. Add a match arm in the `main()` function
3. Implement the command logic

## Common Patterns

### Config Environment Variables

API keys in config support `${ENV_VAR}` syntax. CCR-Rust loads `.env` from the current directory and `~/.claude-code-router/.env`, resolving placeholders at startup.

### Tier Route Format

Tiers are specified as `"provider_name,model_id"` strings. Resolution splits on comma and looks up the provider by name.

### Error Handling

- Use `anyhow::Result` for fallible operations
- HTTP handlers return appropriate status codes (the router maps provider errors to client-facing errors)
- Rate limit 429s are **passed through** to the client (not cascaded internally) — see 429 Pass-Through below
- Non-rate-limit failures (5xx, timeouts) still cascade to the next tier

### Metrics

Add new metrics in `metrics/mod.rs` using `lazy_static!` + prometheus macros. Existing patterns:

- `IntCounterVec` for counts by label (tier, reason)
- `HistogramVec` for distributions (latency buckets)
- `IntGauge` for current values (active streams)

## Testing

Tests are in `/tests/` (integration) and inline `#[cfg(test)]` modules.

```bash
cargo test                           # All tests
cargo test --test integration_codex  # Specific integration test
cargo test -- --nocapture            # Show println output
```

Integration tests use `wiremock` to mock upstream providers.

## Config Files

| File                                | Purpose                                                        |
| ----------------------------------- | -------------------------------------------------------------- |
| `config.example.json`               | Starter template — copy to `~/.claude-code-router/config.json` |
| `config.gemini.json`                | Multi-backend config with Gemini                               |
| `~/.claude-code-router/config.json` | Deployed config (not in repo)                                  |
| `~/.claude-code-router/.env`        | API keys loaded at startup                                     |

## 429 Pass-Through (v1.3.0)

As of v1.3.0, CCR-Rust **passes 429 responses through** to the client instead of silently cascading to the next tier. This is a deliberate architectural choice:

- **Dispatch layer** (`router/dispatch.rs`): On 429, reads the upstream body+headers, normalizes the error body (sets `type: "rate_limit_error"`, `code: "rate_limited"`), forwards allowed upstream headers (filtering hop-by-hop and `content-length`/`content-encoding`), stamps `x-ccr-tier`, and returns `Ok(Response)` instead of `Err(RateLimited)`.
- **Coordination layer** (`router/mod.rs`): Detects 429 status on the `Ok` path, records EWMA failure, and returns immediately — no cascade.
- **All-tiers-exhausted** now only fires for non-rate-limit failures and always returns 503.

### Lessons from 429 refactor

- **Return-type trick**: Dispatch returns `Ok(Response)` with status 429 instead of `Err(RateLimited)`. The coordinator checks `response.status() == 429` on the success path. This avoids threading body bytes through the error enum.
- **Header forwarding requires a deny-list**: Forward all upstream headers except hop-by-hop (RFC 7230 §6.1), representation headers that go stale (`content-length`, `content-encoding`), and CCR's own (`x-ccr-tier`). Use `should_forward_429_header()` for this.
- **`honor_ratelimit_headers` defaults to `true`**: Proactively skip tiers reporting `X-RateLimit-Remaining: 0` on 200s. Set `false` only for providers (like Z.AI) that send informational headers without enforcement.

## Pitfalls

- **Binary sync**: After `cargo build --release`, always run `cargo install --path . --force`. The server uses `target/release/ccr-rust` but CLI commands like `dashboard` and `version` use `~/.cargo/bin/ccr-rust`. If these diverge, behavior is inconsistent.
- **Transformer ordering**: Transformers apply in the order listed in `"use": [...]`. For providers that need both protocol translation AND provider-specific normalization, protocol translation (`"anthropic"`) typically goes first.
- **Streaming vs non-streaming**: `forceNonStreaming: true` is recommended for agent workloads. Interactive use should leave it `false`.
- **MCP compression levels**: `full` compression can strip too much context from tool descriptions. Use `medium` for production.

## File Size Limits (SLOC Guard)

CCR-Rust enforces source lines of code limits to prevent files from growing into unmanageable monoliths. Configuration: `.sloc-guard.toml`.

| Threshold  | Lines (SLOC) | Action                                 |
| ---------- | ------------ | -------------------------------------- |
| Warning    | 600          | Plan a split — file is getting complex |
| Hard limit | 750          | Must split before merging              |
| Tests      | 1000         | Test files get more headroom           |

**SLOC = source lines only** (comments and blank lines excluded).

**Methodology** (based on matklad's "minimize the cut" principle):

- Split when a file contains multiple independent subsystems (not just because it's long)
- Optimize the ratio of module size to interface — a 500-line module with 2 public functions is better than 5 files of 100 lines each re-exporting everything
- When splitting: create `module_name/mod.rs` + submodules, re-export public API from `mod.rs`
- Existing oversized files are grandfathered with a ratchet — they can only shrink, never grow

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).
