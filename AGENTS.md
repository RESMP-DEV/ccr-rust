# CCR-Rust — Repository Guide

> `CLAUDE.md` is a symlink to this file. Use this as the canonical local guide.

## Project Scope

CCR-Rust is a multi-protocol LLM router for Claude Code, Codex, OpenAI-compatible clients, and optional MCP aggregation. It handles protocol translation, transformer pipelines, routing, metrics, and streaming.

## Environment and Setup

Use the Rust-native workflow.

- `cargo check`
- `cargo fmt`
- `cargo clippy`
- `cargo test`
- `cargo build --release`

After code changes, run `cargo install --path . --force` so the installed CLI binary stays in sync with the freshly built one.

For a clean local setup:

- copy `config.example.json` to `~/.claude-code-router/config.json`,
- add API keys via environment-backed placeholders,
- start the router with `ccr-rust start`,
- verify with `ccr-rust status`, `ccr-rust validate`, or `ccr-rust dashboard`.

## Repository Layout

Important files and areas:

- `src/main.rs` — CLI entry point.
- `src/config/` — configuration parsing, pricing, and provider protocol definitions.
- `src/router/` — HTTP handlers, dispatch, request translation, response translation, and streaming.
- `src/gp_router.rs` — request-aware GP reranking and credible-set cost ordering.
- `src/frontend/` — client-format normalization.
- `src/transform/` and `src/transformer.rs` — transformer implementations and registry plumbing.
- `src/metrics/` — Prometheus metrics and persistence.
- `src/mcp/` — MCP server and tool-catalog handling.
- `src/ratelimit.rs` — provider backoff logic.
- `vendor/gp-routing/` — pinned Apache-2.0 GP surrogate source used by standard builds.
- `tests/` — integration coverage.

## Working Rules

Keep these invariants intact:

- Standard OpenAI- or Anthropic-compatible upstreams should be added through config first; only add code when a provider truly needs normalization or a new protocol.
- If you add a transformer, register it in both registry construction paths.
- Keep frontend detection and serialization aligned when adding client-specific behavior.
- Use environment-backed config placeholders instead of embedding secrets.
- Preserve the existing rate-limit and retry model unless the change is explicitly about routing semantics.
- Treat 429s as tracked failures: stop retries for that tier, continue to later eligible tiers, and synthesize a 429 only when every candidate is rate-limited.
- When direct or search routing pins entries at the front, update `pinned_prefix_len`; GP reranking must never move the pinned prefix.
- When `AppState` gains a field, update every integration-test helper that constructs it directly.
- Keep `vendor/gp-routing/NOTICE` accurate when modifying the vendored encoder or surrogate boundary.
- Keep the optional sindexer Git revision pinned to a reviewed public commit;
  standalone CCR-Rust clones must not require an adjacent repository.

## Common Change Patterns

For new providers:

- use config-only wiring when the upstream speaks a supported protocol,
- add a transformer only when request or response normalization is required,
- and add a new protocol variant only when the upstream cannot fit the current transport model.

For new CLI commands:

- add the command enum variant,
- hook it into the top-level dispatch,
- and keep command behavior consistent with the existing subcommand style.

## Validation Expectations

Run at least the relevant unit or integration tests for the area you changed. For routing, translation, or config work, `cargo test --all-features` is usually the minimum acceptable validation path. Changes under `vendor/gp-routing/` also run that package's own tests.

Integration tests use mocked upstreams, so prefer updating those over inventing ad hoc manual checks.

## Pitfalls

Watch out for these recurring mistakes:

- forgetting the post-build `cargo install --path . --force`,
- registering a transformer in only one registry path,
- relying on the wrong transformer order,
- and changing streaming behavior without checking both streaming and non-streaming call paths.

## File Growth Policy

This repo enforces a SLOC ratchet. Split files when they accumulate multiple independent concerns, and prefer clear module boundaries over one giant orchestration file.

## Change log expectations

Update `CHANGELOG.md` when a change affects routing behavior, protocol support, operator workflow, validation posture, or dependency shape. For upstream or sync-heavy work, summarize the effect in plain language rather than copying commit logs.
