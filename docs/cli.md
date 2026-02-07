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

### `version`
Show version and build information.

```bash
ccr-rust version
```

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

# Validate configuration
ccr-rust validate --config ~/custom.toml

# Show version
ccr-rust version

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
| `/v1/messages` | POST | Chat completions API (Anthropic-compatible) |
| `/v1/presets` | GET | List available routing presets |
| `/preset/:preset_name/v1/messages` | POST | Chat completions using a specific preset |
| `/v1/latencies` | GET | Latency metrics per backend |
| `/v1/usage` | GET | Usage statistics |
| `/v1/token-drift` | GET | Token drift metrics |
| `/v1/token-audit` | GET | Recent pre-request token audit entries |
| `/v1/frontend-metrics` | GET | Per-frontend request/latency metrics |
| `/health` | GET | Health check |
| `/metrics` | GET | Prometheus-style metrics |

## Signals

- `SIGINT` (Ctrl+C): Triggers graceful shutdown
- `SIGTERM` (Unix): Triggers graceful shutdown

The server will drain existing connections up to the `--shutdown-timeout` limit before exiting.
