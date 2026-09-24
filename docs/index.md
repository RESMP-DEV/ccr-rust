# CCR-Rust Documentation

Task-oriented index for all CCR-Rust documentation.

Start with [Local operation and agent use](local-operation.md) for opt-in
launches, the existing workstation helpers, portable setup, and verification.
For separate personal and corporate plans, use [Authentication suites](auth-suites.md).
Repository agents also read [AGENTS.md](../AGENTS.md).

To choose Kimi or GLM for a session, see [Switching coding plans](switching-plans.md)
for launch commands, Codex profiles, presets, direct connections, and fallback.

## Setup

1. [CLI commands](cli.md) — start, status, validate, dashboard, captures, mcp, mcp-daemon, and more
2. [Configuration](configuration.md) — providers, API keys, environment variables, full schema
3. [Examples](../examples/README.md) — copy-paste minimal and multi-tier configs, plus a smoke-test script
4. [Presets](presets.md) — one-command setups for common scenarios
5. [Deployment](deployment.md) — multi-machine, systemd/launchd, Docker
6. [Troubleshooting](troubleshooting.md) — common issues, debug tips, logs

## Client Integrations

- [Z.AI and MiniMax Setup](zai_minimax_setup.md) — coding-plan endpoints, GLM-5.3, and protocol choices
- [Claude Code](claude_code_setup.md) — opt-in routing, provider credentials, and tool verification
- [Codex](codex_setup.md) — Codex CLI routing
- [OpenAI SDK](openai_sdk_setup.md) — Python/JavaScript OpenAI client setup
- [Kimi](kimi_setup.md) — Kimi Code keys, current model IDs, and Claude/Codex requirements
- [Gemini](gemini-integration.md) — Google Gemini routing
- [Z.AI Anthropic endpoint](zai_anthropic_endpoint.md) — verified wire contract, image handling, vision caveats, and the credential-safe restart runbook

## Operations

- [Local operation](local-operation.md) — launchers, service files, setup, and dated live-client acceptance
- [Authentication suites](auth-suites.md) — separate API plans and native Claude/ChatGPT logins
- [Switching plans](switching-plans.md) — Kimi/GLM selection, profiles, presets, and fallback
- [Observability](observability.md) — Prometheus metrics, TUI dashboard, token/latency tracking
- [Dependency security](dependency-security.md) — resolved advisories and temporary transitive-risk decisions
- [Debug capture](debug_capture.md) — capture requests/responses for troubleshooting
- [Streaming design](streaming_incremental_design.md) — how streaming responses are handled
- [Token optimization](token_optimization.md) — KimiTransformer, output_compress, semantic hints

## Reference

- [Lessons learned](lessons_learned.md) — past issues, fixes, gotchas
