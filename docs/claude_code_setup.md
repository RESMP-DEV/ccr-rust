# Claude Code fallback how-to

This guide is for the most common real-world question:

> My Claude plan ran out. How do I keep using Claude Code without changing my whole workflow?

CCR-Rust lets you keep **Claude Code as the interface** while routing requests to one or more backup providers behind the scenes.

## The short version

1. Run CCR-Rust locally.
2. Give it one or two provider API keys.
3. Point Claude Code at `http://127.0.0.1:3456`.
4. Keep typing `claude` like normal.

Claude Code still speaks Anthropic format. CCR-Rust receives that request, picks a provider from your config, translates if needed, and returns a response in the format Claude Code expects.

## Step 1: install CCR-Rust

```bash
cargo build --release
cargo install --path . --force
```

If you have not installed Claude Code yet:

```bash
npm install -g @anthropic-ai/claude-code
claude --version
```

## Step 2: create a simple fallback config

Create the default config directory and copy the example file:

```bash
mkdir -p ~/.claude-code-router
cp config.example.json ~/.claude-code-router/config.json
```

Then start with a **minimal backup-provider setup** like this:

```json
{
  "Providers": [
    {
      "name": "deepseek",
      "api_base_url": "https://api.deepseek.com",
      "api_key": "${DEEPSEEK_API_KEY}",
      "models": ["deepseek-chat", "deepseek-reasoner"]
    },
    {
      "name": "openrouter",
      "api_base_url": "https://openrouter.ai/api/v1",
      "api_key": "${OPENROUTER_API_KEY}",
      "models": [
        "inclusionai/ling-2.6-flash:free",
        "minimax/minimax-m2.5:free"
      ],
      "transformer": {
        "use": ["anthropic", "openrouter"]
      }
    }
  ],
  "Router": {
    "default": "deepseek,deepseek-chat",
    "tiers": [
      "deepseek,deepseek-chat",
      "openrouter,inclusionai/ling-2.6-flash:free"
    ]
  },
  "PORT": 3456,
  "HOST": "127.0.0.1"
}
```

### What this config means

- `Providers` lists the upstream APIs CCR-Rust is allowed to call.
- `Router.default` is the first provider/model it should try.
- `Router.tiers` is the fallback order.
- The `openrouter` transformer chain tells CCR-Rust how to adapt Anthropic-style Claude Code requests for OpenRouter.

If you only want one backup provider at first, that is fine. Start simple.

## Step 3: add your provider keys

Use environment variables in your shell or in a local `.env` file that CCR-Rust can load.

Example:

```bash
export DEEPSEEK_API_KEY="your-deepseek-key"
export OPENROUTER_API_KEY="your-openrouter-key"
```

CCR-Rust expands `${ENV_VAR}` placeholders from the config file at startup.

## Step 4: start CCR-Rust

```bash
ccr-rust start
```

Quick checks:

```bash
ccr-rust status
curl http://127.0.0.1:3456/health
```

Expected health response:

```text
ok
```

## Step 5: point Claude Code at CCR-Rust

```bash
export ANTHROPIC_BASE_URL="http://127.0.0.1:3456"
claude
```

If your Claude Code version also insists on `ANTHROPIC_API_KEY` being present locally, keep that variable set too. CCR-Rust still uses the provider keys from `~/.claude-code-router/config.json`.

## Step 6: what happens now?

When you run `claude`, the flow looks like this:

1. Claude Code sends an Anthropic-style request to CCR-Rust.
2. CCR-Rust checks your routing config.
3. It tries the first configured provider/model.
4. If the upstream API uses a different format, CCR-Rust translates the request.
5. It returns a Claude-compatible response back to Claude Code.

So from your point of view, **you are still using Claude Code**. CCR-Rust is just the local layer deciding which model actually answers.

## Step 7: get maximum value after Claude usage limits hit

The easiest pattern is:

- put a **cheap or free model first** for routine work,
- keep a **better fallback model second** for when the first one is overloaded or weak,
- and keep using Claude Code as the front-end you already know.

Example mindset:

- everyday edits: `deepseek,deepseek-chat`
- backup: `openrouter,inclusionai/ling-2.6-flash:free`

You can refine later. The important part is that you do **not** need to learn a new client every time you change providers.

## Optional: prefer Anthropic first, then fall back

If you also want CCR-Rust to try Anthropic first when available, add an Anthropic provider explicitly and mark it as Anthropic protocol:

```json
{
  "name": "anthropic",
  "api_base_url": "https://api.anthropic.com/v1/messages",
  "api_key": "${ANTHROPIC_API_KEY}",
  "protocol": "anthropic",
  "models": ["claude-3-5-sonnet-20241022"]
}
```

Then put it first in `Router.default` / `Router.tiers`.

This is optional. Many people will get value from CCR-Rust even without using Anthropic as an upstream at all.

## Common questions

### “Why not just use another client directly?”

Because the value here is keeping one interface:

- same Claude Code workflow,
- same editor habits,
- same commands,
- different providers behind the curtain.

### “What does failover actually mean?”

If the first provider errors or times out, CCR-Rust can try the next configured tier.

That is why giving it at least two choices is useful.

### “Do I need a huge config?”

No. Start with one provider, then add one fallback. You can make it fancy later.

## Troubleshooting

### CCR-Rust is not running

```bash
ccr-rust status
curl http://127.0.0.1:3456/health
```

If the health check fails, start it again:

```bash
ccr-rust start
```

### Claude Code cannot connect

Check your base URL:

```bash
echo "$ANTHROPIC_BASE_URL"
```

For this guide it should be:

```bash
http://127.0.0.1:3456
```

### The provider key is missing or wrong

Validate the config structure:

```bash
ccr-rust validate
```

Then make sure the provider environment variables are actually set:

```bash
echo "$DEEPSEEK_API_KEY"
echo "$OPENROUTER_API_KEY"
```

### I want to see what CCR-Rust is doing

Use the built-in status and observability tools:

```bash
ccr-rust status
ccr-rust dashboard
curl http://127.0.0.1:3456/metrics
```

## Where to go next

- [Configuration](./configuration.md) — full config reference
- [CLI reference](./cli.md) — available commands
- [Troubleshooting](./troubleshooting.md) — deeper operational fixes
- [Observability](./observability.md) — dashboard and metrics
