# CLI Reference

## Overview

`ccr-rust` is the Claude Code Router server written in Rust. It provides request routing, rate limiting, and metrics collection for multiple LLM API backends.

## Usage

```bash
ccr-rust [GLOBAL_OPTIONS] <COMMAND> [COMMAND_OPTIONS]
```

## Global Options

| Option | Short | Environment | Default | Description |
|--------|-------|-------------|---------|-------------|
| `--config` | `-c` | `CCR_CONFIG` | `~/.claude-code-router/config.json` | Path to CCR config file |

## Commands

### `start` (default)
Start the CCR server. This is the default command if no subcommand is specified.

```bash
ccr-rust start [OPTIONS]
```

| Option | Short | Environment | Default | Description |
|--------|-------|-------------|---------|-------------|
| `--host` | - | - | `127.0.0.1` | Server host to bind to |
| `--port` | `-p` | - | `3456` | Server port |
| `--max-streams` | - | `CCR_MAX_STREAMS` | `512` | Maximum concurrent streams (0 = unlimited) |
| `--shutdown-timeout` | - | - | `30` | Graceful shutdown timeout in seconds |

### `status`
Check if the CCR server is running.

```bash
ccr-rust status [OPTIONS]
```

| Option | Short | Default | Description |
|--------|-------|---------|-------------|
| `--host` | - | `127.0.0.1` | Server host to check |
| `--port` | `-p` | `3456` | Server port to check |

### `validate`
Validate configuration file syntax and providers.

```bash
ccr-rust validate
```

### `dashboard`
Launch the interactive TUI dashboard (requires the default `dashboard` feature).

```bash
ccr-rust dashboard [OPTIONS]
```

| Option | Short | Environment | Default | Description |
|--------|-------|-------------|---------|-------------|
| `--host` | - | `CCR_DASHBOARD_HOST` | `127.0.0.1` | Router host to connect to |
| `--port` | `-p` | `CCR_DASHBOARD_PORT` | `3456` | Router port to connect to |

### `version`
Show version and build information.

```bash
ccr-rust version
```

### `captures`
List and analyze debug captures (requires `DebugCapture.enabled=true` in config).

```bash
ccr-rust captures [OPTIONS]
```

| Option | Short | Default | Description |
|--------|-------|---------|-------------|
| `--provider` | `-p` | - | Filter by provider name |
| `--limit` | `-l` | `20` | Maximum captures to list |
| `--stats` | - | `false` | Show aggregate statistics instead of individual captures |
| `--output-dir` | - | config value | Capture directory override |
| `--full` | - | `false` | Show full request/response bodies |

### `mcp`
Run as a stdio MCP (Model Context Protocol) server, optionally wrapping other MCP backends.

```bash
ccr-rust mcp [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--level` | Log level (default `low`) |
| `--wrap <backend>` | Wrap another MCP backend (repeatable) |
| `--include <tools>` | Comma-separated tool allowlist |
| `--exclude <tools>` | Comma-separated tool denylist |

### `mcp-daemon`
Run as a shared MCP daemon over HTTP with native tools. Requires a bearer token.

```bash
ccr-rust mcp-daemon [OPTIONS] --auth-token <TOKEN>
```

| Option | Short | Environment | Default | Description |
|--------|-------|-------------|---------|-------------|
| `--port` | `-p` | - | `3457` | Daemon port |
| `--host` | - | `CCR_MCP_DAEMON_HOST` | `127.0.0.1` | Listen address |
| `--auth-token` | - | `CCR_MCP_AUTH_TOKEN` | - | Required bearer token (prefer the env var) |
| `--memory-dir` | - | `CCR_MCP_MEMORY_DIR` | - | Directory for memory graph persistence |
| `--pyright-root` | - | `PYRIGHT_PROJECT_ROOT` | - | Project root for Pyright type-checking |
| `--pyright-workspace-dir` | - | `CCR_MCP_PYRIGHT_WORKSPACE_DIR` | - | Private directory for ephemeral Pyright workspaces |

### `clear-stats`
Delete persisted CCR observability stats from Redis for one prefix.

```bash
ccr-rust clear-stats [OPTIONS]
```

| Option | Environment | Default | Description |
|--------|-------------|---------|-------------|
| `--redis-url` | `CCR_REDIS_URL` | `Persistence.redis_url` | Redis URL to connect to |
| `--redis-prefix` | - | `Persistence.redis_prefix` | Prefix namespace to delete |

## Examples

```bash
# Start with default settings
ccr-rust start

# Start with custom host and port
ccr-rust start --host 0.0.0.0 --port 8080

# Start with unlimited concurrent streams
ccr-rust start --max-streams 0

# Start with extended shutdown timeout
ccr-rust start --shutdown-timeout 60

# Use custom config file
ccr-rust -c /etc/ccr/config.json start

# Check server status
ccr-rust status

# Validate an alternate config file
ccr-rust --config ~/custom-config.json validate

# Show version
ccr-rust version

# Dashboard against a remote router
ccr-rust dashboard --host 10.0.0.5 --port 3456

# Recent captures for one provider, with full bodies
ccr-rust captures --provider minimax --limit 10 --full

# MCP daemon with bearer auth via env var
CCR_MCP_AUTH_TOKEN="a-private-random-token" ccr-rust mcp-daemon

# Clear persisted stats using config persistence settings
ccr-rust clear-stats

# Clear with explicit Redis target
ccr-rust clear-stats --redis-url redis://127.0.0.1:6379/0 --redis-prefix ccr-rust:persistence:v1
```

## Redis Persistence

To keep observability data across CCR restarts (dashboard usage, token drift, and restored histogram offsets), add:

```json
"Persistence": {
  "mode": "redis",
  "redis_url": "redis://127.0.0.1:6379/0",
  "redis_prefix": "ccr-rust:persistence:v1"
}
```

Notes:
- `mode`: `none` (default) or `redis`
- `redis_url`: required when `mode=redis` (or set `CCR_REDIS_URL`)
- `redis_prefix`: Redis key namespace for CCR persistence records

## HTTP Endpoints

Once running, the server exposes the following endpoints:

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v1/messages` | POST | Messages API (Anthropic-compatible) |
| `/v1/chat/completions` | POST | Chat completions API (OpenAI-compatible) |
| `/v1/responses` | POST | Responses API (OpenAI-compatible, streaming) |
| `/v1/models` | GET | List configured models |
| `/v1/presets` | GET | List available routing presets |
| `/preset/:preset_name/v1/messages` | POST | Messages using a specific preset |
| `/v1/latencies` | GET | Latency metrics per backend |
| `/v1/usage` | GET | Usage statistics |
| `/v1/token-drift` | GET | Token drift metrics |
| `/v1/token-audit` | GET | Recent pre-request token audit entries |
| `/v1/throughput` | GET | Throughput statistics |
| `/v1/frontend-metrics` | GET | Per-frontend request/latency metrics |
| `/health` | GET | Health check |
| `/metrics` | GET | Prometheus-style metrics |

## Signals

- `SIGINT` (Ctrl+C): Triggers graceful shutdown
- `SIGTERM` (Unix): Triggers graceful shutdown

The server will drain existing connections up to the `--shutdown-timeout` limit before exiting.
