# CCR-Rust Examples

Copy-paste starting points for common setups. All configs use `${ENV_VAR}`
placeholders — export the variables (or pass them via your service manager)
before starting the router.

| File | What it shows |
| ---- | ------------- |
| [`config.minimal.json`](config.minimal.json) | Smallest useful config: one provider, one route. |
| [`config.followers.json`](config.followers.json) | Recommended multi-provider starting point: GLM coding plan primary, MiniMax second tier, NVIDIA NIM free overflow, retry sweeps enabled with a bounded hold. |
| [`config.multitier.json`](config.multitier.json) | Three-provider failover cascade, traffic-class routes, per-tier retries, and presets. |
| [`coding-plans.json`](coding-plans.json) | Explicit Kimi/GLM selection, named Anthropic presets, and Kimi's required output limit for Codex. |
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

# 3. Or the recommended multi-provider setup: GLM plan + MiniMax + free NIM overflow,
#    with retry sweeps holding exhausted requests instead of failing them.
#    Stop the step-1 router first — every example binds the same 127.0.0.1:3456:
kill %1 2>/dev/null || pkill -f 'ccr-rust start' || true
export ZAI_API_KEY="..." MINIMAX_API_KEY="..." NVIDIA_API_KEY="..."
ccr-rust --config examples/config.followers.json validate
ccr-rust --config examples/config.followers.json start

# 4. Or run the multi-tier config without touching your default one
#    (stop the followers router first — same shared port)
kill %1 2>/dev/null || pkill -f 'ccr-rust start' || true
export ZAI_API_KEY="..." MINIMAX_API_KEY="..." DEEPSEEK_API_KEY="..."
ccr-rust --config examples/config.multitier.json validate
ccr-rust --config examples/config.multitier.json start
```

For the full config schema and every supported field, see
[docs/configuration.md](../docs/configuration.md). For provider-specific
setup guides (Claude Code, Codex, Kimi, Gemini, Z.AI/MiniMax), see
[docs/index.md](../docs/index.md).

For the Kimi/GLM example, load `KIMI_API_KEY` and `ZAI_API_KEY`, then follow
[Switching coding plans](../docs/switching-plans.md). Use keys from the intended
plan; Kimi Code and Moonshot Platform keys are distinct. The example deliberately
does not put both providers in a shared fallback list.
