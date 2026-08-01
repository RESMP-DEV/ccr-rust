# CCR-Rust Examples

Copy-paste starting points for common setups. All configs use `${ENV_VAR}`
placeholders — export the variables (or pass them via your service manager)
before starting the router.

| File | What it shows |
| ---- | ------------- |
| [`config.minimal.json`](config.minimal.json) | Smallest useful config: one provider, one route. |
| [`config.multitier.json`](config.multitier.json) | Three-provider failover cascade, traffic-class routes, per-tier retries, and presets. |
| [`smoke-test.sh`](smoke-test.sh) | Checks a running router end-to-end: health, model list, and one Anthropic-style + one OpenAI-style request. |

## Try it

```bash
# 1. Minimal setup
export DEEPSEEK_API_KEY="sk-..."
mkdir -p ~/.claude-code-router
cp examples/config.minimal.json ~/.claude-code-router/config.json
ccr-rust validate
ccr-rust start &

# 2. Verify it works
./examples/smoke-test.sh

# 3. Or run the multi-tier config without touching your default one
export ZAI_API_KEY="..." MINIMAX_API_KEY="..." DEEPSEEK_API_KEY="..."
ccr-rust --config examples/config.multitier.json validate
ccr-rust --config examples/config.multitier.json start
```

For the full config schema and every supported field, see
[docs/configuration.md](../docs/configuration.md). For provider-specific
setup guides (Claude Code, Codex, Kimi, Gemini, Z.AI/MiniMax), see
[docs/index.md](../docs/index.md).
