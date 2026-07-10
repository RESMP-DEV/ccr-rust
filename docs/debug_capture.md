# Debug Capture

Debug capture records raw request/response data from provider interactions, enabling debugging of provider-specific issues like response drift, malformed outputs, or unexpected behavior. It is explicitly disabled by default because request and response bodies can contain sensitive user data. Enable it only for a bounded debugging session.

## Overview

When enabled, CCR-Rust captures:
- Full request body (JSON payload sent to provider)
- Full response body (raw text received)
- Response status code and latency
- Timestamps and unique request IDs
- Error messages (for failed requests)

Captures are stored as JSON files in a configurable directory, with automatic rotation to prevent disk exhaustion.

## Configuration

Add the `DebugCapture` section to your `config.json`:

```json
{
  "DebugCapture": {
    "enabled": true,
    "providers": ["minimax"],
    "output_dir": "~/.ccr-rust/captures",
    "max_files": 100,
    "capture_success": false,
    "max_body_size": 1048576
  }
}
```

### Configuration Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enabled` | bool | `false` | Enable/disable capture globally |
| `providers` | string[] | `[]` | Provider names to capture. Empty = capture all |
| `output_dir` | string | `~/.ccr-rust/captures` | Output directory (supports `~` expansion) |
| `max_files` | int | `100` | Maximum CCR-owned capture files before rotation (hard maximum: 1000) |
| `capture_success` | bool | `false` | Capture successful responses (not just errors) |
| `max_body_size` | int | `1048576` | Max response body size in bytes; every serialized capture also has a hard 4 MiB file cap |
| `include_headers` | bool | `false` | Include HTTP headers in capture; leave disabled when headers may contain credentials |

On Unix, CCR-Rust enforces mode `0700` on the capture directory and mode
`0600` on every new capture file. Retention runs after every write and removes
only files created with the current `ccr_capture_v1_` prefix. Legacy captures,
task captures, symlinks, and unrelated JSON files are left untouched.
Capture listing and statistics likewise inspect only current-format regular
files, cap each file read at 4 MiB, and return at most 1000 entries.

### Provider Names

Use the provider name from your `Providers` config, not the tier name:

```json
{
  "Providers": [
    { "name": "minimax", "tier_name": "ccr-mm" },
    { "name": "deepseek", "tier_name": "ccr-ds" }
  ],
  "DebugCapture": {
    "providers": ["minimax", "deepseek"]
  }
}
```

## API Endpoints

### GET /debug/capture/status

Returns capture configuration and current statistics.

```bash
curl -s http://localhost:3456/debug/capture/status | jq .
```

Response:
```json
{
  "enabled": true,
  "output_dir": "~/.ccr-rust/captures",
  "providers": ["minimax"],
  "stats": {
    "total_captures": 42,
    "success_count": 38,
    "error_count": 4,
    "avg_latency_ms": 2847,
    "by_provider": {
      "minimax": 42
    }
  }
}
```

### GET /debug/capture/list

Lists recent captures with optional filtering.

Query parameters:
- `provider` - Filter by provider name (optional)
- `limit` - Maximum captures to return (default: 20)

```bash
# All recent captures
curl -s "http://localhost:3456/debug/capture/list?limit=10" | jq .

# Filter by provider
curl -s "http://localhost:3456/debug/capture/list?provider=minimax&limit=5" | jq .
```

### GET /debug/capture/stats

Returns detailed statistics about captured interactions.

```bash
curl -s http://localhost:3456/debug/capture/stats | jq .
```

## CLI Commands

The `ccr-rust captures` command provides CLI access to capture data:

```bash
# List recent captures
ccr-rust captures --limit 10

# Filter by provider
ccr-rust captures --provider minimax --limit 20

# Show statistics only
ccr-rust captures --stats

# Show full response bodies
ccr-rust captures --full

# Custom output directory
ccr-rust captures --output-dir /tmp/debug-captures
```

## Capture File Format

Captures are stored as JSON files with the naming convention:
```
{provider}_{tier_name}_{timestamp}_{request_id}.json
```

Example: `minimax_ccr-mm_20260209_183742_00001234.json`

### File Structure

```json
{
  "request_id": 1234,
  "provider": "minimax",
  "tier_name": "ccr-mm",
  "model": "MiniMax-M2.5",
  "timestamp": "2026-02-09T18:37:42.123456Z",
  "url": "https://api.minimax.io/anthropic/v1/messages",
  "method": "POST",
  "request_body": {
    "model": "MiniMax-M2.5",
    "messages": [...],
    "max_tokens": 4096
  },
  "response_status": 200,
  "response_body": "{\"id\":\"msg_...\",\"content\":[...]}",
  "response_truncated": false,
  "latency_ms": 2847,
  "is_streaming": false,
  "success": true,
  "error": null
}
```

### Fields

| Field | Description |
|-------|-------------|
| `request_id` | Unique ID for this request |
| `provider` | Provider name (e.g., "minimax") |
| `tier_name` | Tier display name (e.g., "ccr-mm") |
| `model` | Model name used |
| `timestamp` | ISO 8601 timestamp |
| `url` | Request URL |
| `method` | HTTP method |
| `request_body` | Full request payload as JSON |
| `response_status` | HTTP status code (0 if connection failed) |
| `response_body` | Raw response text |
| `response_truncated` | Whether body was truncated due to `max_body_size` |
| `latency_ms` | Request duration in milliseconds |
| `is_streaming` | Whether this was a streaming request |
| `success` | True if status was 2xx |
| `error` | Error message for failed requests |

## Use Cases

### Debugging Response Drift

Capture responses over time to identify when provider behavior changes:

```bash
# Enable capture for a specific provider
# Edit config.json: "providers": ["minimax"]

# After running tasks, analyze captures
ccr-rust captures --provider minimax --limit 100 --full > minimax_samples.json

# Compare response patterns
jq '.[].response_body | fromjson | .content[0].text | length' minimax_samples.json
```

### Diagnosing Failures

Capture error responses for debugging:

```bash
# List captures with errors
curl -s "http://localhost:3456/debug/capture/list?limit=50" | \
  jq '[.captures[] | select(.success == false)]'
```

### Latency Analysis

Analyze response times:

```bash
# Get latency distribution
curl -s "http://localhost:3456/debug/capture/list?limit=100" | \
  jq '[.captures[].latency_ms] | sort | {min: .[0], max: .[-1], median: .[length/2|floor]}'
```

## File Rotation

CCR-Rust automatically rotates its own current-format capture files as soon as
`max_files` is exceeded. `max_files=0` is not an unlimited mode; it is replaced
with the bounded default of 100 files.

1. Rotation runs after every successful capture write
2. Files are sorted by modification time
3. Oldest current-format regular files are deleted until under the limit
4. If the limit cannot be enforced, the newly written capture is removed

To manually clean up captures:

```bash
# Remove all current-format CCR captures (preserves legacy and task files)
find ~/.ccr-rust/captures -type f -name 'ccr_capture_v1_*.json' -delete

# Remove current-format CCR captures older than 7 days
find ~/.ccr-rust/captures -type f -mtime +7 -name 'ccr_capture_v1_*.json' -delete
```

## Performance Impact

Debug capture adds bounded disk I/O to captured requests:
- Capture work runs only after an explicit opt-in
- Each file is synchronously persisted before the request finishes
- Serialized files and retained file counts have hard bounds

For high-throughput scenarios, consider:
- Limiting to specific providers
- Reducing `max_files`
- Disabling `capture_success` to only capture errors

## Disabling Capture

To disable without removing the config:

```json
{
  "DebugCapture": {
    "enabled": false
  }
}
```

Or remove the `DebugCapture` section entirely, then restart CCR-Rust.
