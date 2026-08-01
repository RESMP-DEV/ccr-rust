# CCR-Rust Documentation

Task-oriented index for all CCR-Rust documentation.

## Setup

1. [CLI commands](cli.md) — start, status, validate, dashboard, captures, mcp, mcp-daemon, and more
2. [Configuration](configuration.md) — providers, API keys, environment variables, full schema
3. [Examples](../examples/README.md) — copy-paste minimal and multi-tier configs, plus a smoke-test script
4. [Presets](presets.md) — one-command setups for common scenarios
5. [Deployment](deployment.md) — multi-machine, systemd/launchd, Docker
6. [Troubleshooting](troubleshooting.md) — common issues, debug tips, logs

## Client Integrations

- [Z.AI and MiniMax Setup](zai_minimax_setup.md) — modern GLM-5.2 and MiniMax-M3 providers
- [Claude Code fallback how-to](claude_code_setup.md) — step-by-step guide for keeping Claude Code useful after Claude usage limits kick in
- [Codex](codex_setup.md) — Codex CLI routing
- [OpenAI SDK](openai_sdk_setup.md) — Python/JavaScript OpenAI client setup
- [Kimi](kimi_setup.md) — Kimi K2.5, token optimization, thinking blocks
- [Gemini](gemini-integration.md) — Google Gemini routing

## Operations

- [Observability](observability.md) — Prometheus metrics, TUI dashboard, token/latency tracking
- [Debug capture](debug_capture.md) — capture requests/responses for troubleshooting
- [Streaming design](streaming_incremental_design.md) — how streaming responses are handled
- [Token optimization](token_optimization.md) — KimiTransformer, output_compress, semantic hints

## Reference

- [Lessons learned](lessons_learned.md) — past issues, fixes, gotchas
