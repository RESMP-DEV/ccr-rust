# Changelog

All notable changes to CCR-Rust will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Recording rules

- Keep `Unreleased` current.
- Record routing, protocol, operator workflow, dependency, and validation changes.
- Summarize the effect of syncs or upstream pulls instead of pasting commit logs.

## [Unreleased]

### Added

- **Per-request token audit on `/v1/token-audit`** — New read-only endpoint
  exposing recent per-request telemetry (tier, token counts, drift against the
  pre-request estimate) so consumers can correlate individual requests with the
  aggregate `/v1/usage` counters.

- **Cost reporting on `/v1/usage`** — The usage summary now returns
  `total_cost_usd` and a per-tier `cost_usd`, computed at request time from the
  provider's resolved model pricing (`pricing_for_model`) as each response is
  recorded. A new `ccr_cost_usd_total` counter is tracked per tier and persisted
  and restored alongside the token counters, so cost survives restarts. Unpriced
  tiers contribute nothing, making the total a lower bound when some providers
  lack pricing.
- **Self-contained GP routing** — Vendored the Apache-2.0 `gp-routing` crate at
  its pinned upstream commit so public builds no longer require access to a
  private repository.
- **Standalone sindexer dependency** — Pin the optional MIT-licensed sindexer
  integration to its public Git commit so CCR-Rust builds no longer require a
  sibling checkout.
- **Provider and model pricing** — Added optional USD-per-million input/output
  pricing with model-specific overrides and continuous per-request cost
  features for GP routing.

- **Z.AI GLM-5.2 with reasoning_effort** — Added GLM-5.2 model with configurable
  `reasoning_effort` parameter (`low`, `medium`, `high`). The `glm` transformer now
  injects `reasoning_effort=medium` by default for GLM-5.2/5.1/5-turbo models, enabling
  control over reasoning depth while maintaining backward compatibility with legacy
  `` tag extraction.

- **MiniMax M3 with adaptive thinking** — Added MiniMax-M3 model with native Anthropic-style
  `thinking` blocks support. The `minimax` transformer now distinguishes between M3 models
  (using `thinking: {type: "adaptive"}`) and M2.x models (using `reasoning_split: true`),
  with proper handling of structured `reasoning_content` arrays from the OpenAI-compatible endpoint.

- **MiniMax highspeed variants** — Added MiniMax-M2.7-highspeed and MiniMax-M2.5-highspeed models
  for faster inference while maintaining the same performance characteristics.

- **Provider request transformations** — Both `glm` and `minimax` transformers now include
  `transform_request` methods that strip Anthropic-specific passthrough fields (`metadata`,
  `anthropic-beta`, `anthropic-version`) to ensure clean upstream requests.

### Changed

- **Authenticated native MCP daemon** — `mcp-daemon` now requires a bearer token
  from `--auth-token` or `CCR_MCP_AUTH_TOKEN`, compares presented credentials in
  constant time, and protects both `/health` and `/mcp`.
- **Private configurable Pyright workspaces** — Pyright now requires
  `--pyright-workspace-dir` or `CCR_MCP_PYRIGHT_WORKSPACE_DIR` alongside the
  project root, enforces mode `0700`, and removes stale CCR-owned UUID
  workspaces at daemon startup without traversing symlink targets.
- **GP routing enabled in standard builds** — Added `gp` to the default feature
  set, expanded the encoder and default candidate limit from 8 to 32 routes,
  and reject GP-enabled configuration when using an explicitly GP-free build.
- **Uncertainty-respecting cost ordering** — Cheaper routes are promoted only
  inside the GP posterior quality credible set, avoiding an arbitrary scalar
  exchange rate between predicted quality and dollars.
- **CLI-only debug capture access** — Removed the unauthenticated capture HTTP
  routes. Private captures can now be inspected only from the local
  `ccr-rust captures` command.

- **Default coding model** — Changed default routing from `glm-5.1` to `glm-5.2` for improved
  reasoning and coding performance with configurable reasoning effort.

- **Z.AI model lineup** — Updated Z.AI models to `glm-5.2`, `glm-5.1`, `glm-5-turbo`, `glm-4.7`
  (added `glm-5.2` at the front of the tier ordering).

- **MiniMax model lineup** — Updated MiniMax models to `MiniMax-M3`, `MiniMax-M2.7`,
  `MiniMax-M2.7-highspeed`, `MiniMax-M2.5`, `MiniMax-M2.5-highspeed` (added M3 and highspeed
  variants to the tier ordering).

### Fixed

- **Streamed SSE usage matches recorded usage** — In the OpenAI→Anthropic
  translated streaming path, the prompt-token fallback to the pre-request
  estimate is now applied before the final `message_delta`/stop events are
  serialized. Previously clients could see `input_tokens: 0` in the final SSE
  usage while `/v1/usage` and cost accounting counted the estimated prompt,
  leaving per-response and aggregate accounting inconsistent.

- **Private bounded debug capture** — Raw provider capture remains explicitly
  disabled when configuration is absent, defaults to error-only capture when
  enabled, enforces `0700` directories and `0600` files on Unix, creates files
  without overwriting existing paths, and applies retention after every write
  only to current CCR-owned files. Zero or excessive retention limits are now
  converted to bounded values without deleting legacy captures or unrelated
  files. Body limits are clamped to 2 MiB without splitting UTF-8; optional
  headers are bounded and redact common credential names; HTTP failures, rate
  limits, and provider errors embedded in success responses retain failure
  semantics. Listing and statistics ignore symlinks and legacy files and are
  capped at 100 captures, 4 MiB per file, and 16 MiB per pass.

- **GP configuration validation** — Reject invalid KPLS dimensions during
  startup validation and before fitting so bad configuration cannot trigger
  repeated failed refits, and cap public backend-ranking inputs to the fixed
  32-slot feature capacity.

- **MiniMax structured reasoning handling** — The `minimax` transformer now correctly extracts
  and normalizes reasoning content from MiniMax-M3's structured `reasoning_content` array
  format (returned by the OpenAI-compatible endpoint) into plain text for consistent handling.

- **GLM reasoning_content preservation** — The `glm` transformer now preserves modern Z.AI
  responses that already include `reasoning_content` in the OpenAI format, merging legacy
  `` tag extractions with modern API responses when both are present.

### Added

- **LongCat Thinking response normalization** — Added a `longcat-thinking`
  transformer and changed native Anthropic response dispatch to apply response
  transformers before strict deserialization. This lets CCR-Rust normalize
  LongCat's unsigned `thinking` blocks before OpenAI-compatible clients require
  a `choices[]` response shape.
- **Anthropic Bearer auth provider option** — Added provider-level
  `auth_header = "authorization"` support so Anthropic-compatible upstreams
  that require `Authorization: Bearer` can route through the native Anthropic
  dispatch path without a custom transformer.
- **Centralized Pyright type-checking via MCP daemon** — New `type_check` native tool runs
  Pyright on the hub, eliminating per-worker Pyright/Pylance instances. Workers send file paths
  and optional content overlays; the hub creates ephemeral workspaces, runs `pyright --outputjson`,
  and returns structured diagnostics. Bounded to 3 concurrent invocations via semaphore.
- **`--host` flag for MCP daemon** — Daemon now accepts an explicit bind address while
  keeping the default on `127.0.0.1`; set `--host 0.0.0.0` or `CCR_MCP_DAEMON_HOST`
  to opt into worker access from other machines.
- **`--pyright-root` flag for MCP daemon** — Sets the project root for Pyright workspace
  preparation. Gated: tool only registers when the flag or `PYRIGHT_PROJECT_ROOT` env var is set.

### Fixed

- **Security dependency refresh** — Pinned `openssl` to `0.10.80`, `rand`
  to `0.8.6`, and upgraded `ratatui` to `0.30.0` so the router lockfile can
  resolve away from the current Dependabot advisories affecting `openssl`,
  `rand`, and transitive `lru` usage, including the follow-up AES-KW-PAD
  `CipherCtxRef::cipher_update_inplace` advisory.

- **Codex Responses token limit routing** — `/v1/responses` requests now map
  `max_output_tokens` to OpenAI chat-compatible `max_completion_tokens` instead
  of legacy `max_tokens`, matching newer OpenAI reasoning model requirements.

## [1.3.0] - 2026-04-09

### Changed

- **BREAKING: 429 pass-through instead of internal tier cascade** — When a provider returns
  HTTP 429 (rate limited), ccr-rust now passes the response through to the client with
  normalized error body (`type: "rate_limit_error"`, `code: "rate_limited"`) and upstream
  headers intact, plus an `x-ccr-tier` header identifying which tier was rate-limited.
  Previously, ccr-rust silently cascaded to the next tier internally. This change enables
  external orchestrators and retry-aware clients to make informed routing decisions with
  accurate rate-limit signal. OSS users relying on the internal cascade should implement
  client-side retry logic or configure their proxy to retry on 429.

- **`honor_ratelimit_headers` default changed to `true`** — Per-provider setting now defaults
  to `true`, meaning ccr-rust proactively skips tiers that report `X-RateLimit-Remaining: 0`
  on successful responses. Set to `false` for providers (like Z.AI) that send informational
  rate-limit headers without actually enforcing them.

- **All-tiers-exhausted response simplified to 503** — Since 429s are now passed through at
  the dispatch layer, the "all tiers exhausted" path only fires for non-rate-limit failures
  (5xx, timeouts) and consistently returns HTTP 503 with `server_error` type.

## [1.2.0] - 2026-04-07

### Added

- **Benchmark harness** — Added `criterion` benchmarks (`benches/concurrent_streams.rs`)
- **Crate metadata** — `description`, `repository`, `keywords`, and `categories` in Cargo.toml

### Fixed

- **Pseudo-SSE tool_use and thinking blocks dropped** — `emit_anthropic_sse_events()` in
  `streaming.rs` only handled `Text` content blocks, silently skipping `ToolUse` and `Thinking`
  via `_ => continue`. When `forceNonStreaming: true` is enabled, all non-streaming Anthropic
  responses pass through this function. Any response containing tool calls was converted to SSE
  with `stop_reason: tool_use` but no actual tool_use content blocks, causing Claude CLI to fail
  with `[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=tool_use` (exit code 1).
  Now handles all three `AnthropicContentBlock` variants: `Text` (text_delta), `ToolUse`
  (content_block_start with metadata + input_json_delta), and `Thinking` (thinking_delta +
  signature_delta). Added 3 unit tests.

### Changed

- **Open source cleanup** — Removed internal framework references from documentation and source.
  Added SPDX license headers to all source files.

## [1.1.1] - 2025-02-14

### Added

- **Gemini Integration** — Direct API access to Google Gemini models for context compression
  - New `gemini` provider with OpenAI-compatible endpoint
  - `gemini-3-flash-preview` model support (1M+ token context window)
  - Documentation preset for cost-effective context compression
  - Comprehensive [Gemini Integration Guide](docs/gemini-integration.md)

- **Environment Variable Expansion** — Use `${VAR_NAME}` syntax in config files
  - Automatic `.env` file loading from working directory and `~/.claude-code-router/`
  - Secure API key management without hardcoding

### Changed

- **Removed `longContext` / `longContextThreshold`** — Vestigial feature replaced by explicit presets
  - Use the `documentation` preset for context compression instead
  - Cleaner configuration with explicit routing control

- **Updated Documentation**
  - README.md: Added Gemini to supported providers
  - configuration.md: Added environment variable section, removed longContext
  - presets.md: Added built-in presets documentation

### Cost Savings

With Gemini Flash for context compression:

| Scenario          | Before | After  | Savings   |
| ----------------- | ------ | ------ | --------- |
| 200K → 20K tokens | $0.60  | $0.075 | **87.5%** |

At 1000 requests/day, this saves **$500+/day**.

## [1.1.0] - 2025-02-11

### Added

- **MiniMax M2.5 Support** — Updated to latest MiniMax models
- **Enhanced Transformer Chain** — Improved reasoning extraction for DeepSeek and MiniMax
- **Dashboard Persistence** — Optional Redis-backed metrics storage

### Changed

- **Simplified Configuration** — Removed deprecated fields
- **Improved Error Handling** — Better error messages for configuration issues
- **Documentation Updates** — Clarified preset routing and tier management

## [1.0.0] - 2025-02-01

### Added

- Initial release
- Multi-provider routing with automatic failover
- OpenAI and Anthropic wire format support
- SSE streaming support
- TUI dashboard for monitoring
- Preset-based routing
- Token drift monitoring
- Prometheus metrics
