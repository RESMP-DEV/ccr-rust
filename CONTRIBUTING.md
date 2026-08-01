# Contributing to CCR-Rust

## Getting started

The repository root is the crate — no subdirectory needed:

```bash
cargo check
cargo test
cargo build --release
cargo install --path . --force   # keep the installed CLI in sync with your build
```

See [AGENTS.md](AGENTS.md) for the full working rules and validation expectations.

## Source tree

```
src/
├── main.rs          # CLI entry point, subcommand dispatch
├── lib.rs           # Crate root, pub module exports
├── config/          # Config parsing, pricing, provider protocol definitions
├── router/          # HTTP handlers, dispatch, request/response translation, streaming
├── frontend/        # Client-format normalization (Claude Code, Codex, detection)
├── transform/       # Provider-specific transformers + factory registry
├── transformer/     # Transformer trait, chain, and built-in impls
├── routing.rs       # EWMA latency tracking
├── gp_router.rs     # Request-aware GP reranking (feature = "gp")
├── ratelimit.rs     # Provider backoff / rate-limit tracking
├── metrics/         # Prometheus metrics + Redis persistence
├── mcp/             # MCP stdio server and shared HTTP daemon
├── dashboard.rs     # Interactive TUI dashboard (feature = "dashboard")
└── debug_capture.rs # Request/response capture for troubleshooting
vendor/gp-routing/   # Vendored Apache-2.0 GP surrogate crate
tests/               # Integration tests (mocked upstreams)
```

## Key modules

### `config/`
Loads and validates `~/.claude-code-router/config.json`. Supports `${ENV_VAR}` expansion. Resolves providers, API keys, and optional Redis persistence.

### `frontend/`
Client-format normalization: detects which client is calling (Claude Code, Codex, generic OpenAI SDK) and applies client-specific behavior.

### `router/`
HTTP handlers (Anthropic `/v1/messages`, OpenAI `/v1/chat/completions`, `/v1/responses`), provider dispatch, protocol translation, and streaming. Tier cascade with exponential backoff on 5xx/timeouts; rate-limit-aware tier skipping via `ratelimit.rs`; EWMA latency tracking in `routing.rs`; optional GP-backed reranking in `gp_router.rs`.

### `transformer/`
Interface for request/response transformations (e.g., KimiTransformer for token optimization, thinking blocks).

### `transform/`
Provider-specific transformers: toolcompress (Kimi), output_compress (Kimi), semantic token hints (Gemini), etc.

### `metrics/`
Prometheus collectors: token counts (input/output), latencies (p50/p90/p99), provider response times, circuit-breaker states.

## Testing

```bash
# Run tests
cargo test

# Run a single test
cargo test test_anthropic_routing -- --nocapture

# Check for warnings
cargo clippy

# Format code
cargo fmt
```

## Code style

- Format: `cargo fmt` (Rust standard)
- Linting: `cargo clippy` (no warnings)
- Documentation: `///` comments on public items
- Error handling: `anyhow::Result` for fallible functions

## Adding a new provider

Standard OpenAI- or Anthropic-compatible upstreams need **config only** — no
code changes (see [docs/configuration.md](docs/configuration.md)). Only add
code when a provider truly needs normalization:

1. Add a transformer in `src/transform/` and register it in **both** registry
   construction paths (`src/transform/registry.rs` and `src/transformer/mod.rs`)
2. Add a protocol variant only if the upstream cannot fit the current transport model
3. Update `docs/configuration.md` with API key setup
4. Add integration coverage in `tests/` (mocked upstreams) and run `cargo test --all-features`

## Commit workflow

1. Write a clear commit message (problem → solution)
2. Ensure `cargo test` and `cargo clippy` pass
3. Verify new public items have `///` documentation
4. Push to feature branch, open PR

## Licensing

CCR-Rust is AGPL-3.0-or-later. By contributing, you agree that your changes are licensed under AGPL-3.0-or-later.
