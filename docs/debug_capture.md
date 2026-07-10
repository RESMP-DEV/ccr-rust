# Private Debug Capture

Debug capture records bounded provider request and response material for a
short debugging session. Captures can contain source code, prompts, model
output, and other sensitive user data. The feature is disabled by default and
is available only through the local `ccr-rust captures` CLI; CCR-Rust exposes
no debug-capture HTTP routes.

## Configuration

Add `DebugCapture` to the CCR-Rust configuration only while diagnosing a
specific provider failure:

```json
{
  "DebugCapture": {
    "enabled": true,
    "providers": ["minimax"],
    "output_dir": "~/.ccr-rust/captures",
    "max_files": 100,
    "include_headers": false,
    "capture_success": false,
    "max_body_size": 1048576
  }
}
```

| Option | Default | Contract |
| --- | --- | --- |
| `enabled` | `false` | Capture nothing unless explicitly enabled. |
| `providers` | `[]` | Provider names to capture; empty means every provider. |
| `output_dir` | `~/.ccr-rust/captures` | Private local capture directory. |
| `max_files` | `100` | Retained CCR-owned files; zero becomes 100 and values above 1000 are clamped. |
| `include_headers` | `false` | Store bounded headers after redacting common credential and cookie names. Leave this off unless headers are essential. |
| `capture_success` | `false` | Persist successful non-streaming interactions as well as failures. |
| `max_body_size` | `1048576` | Captured response bytes; zero becomes 1 MiB and values above 2 MiB are clamped. UTF-8 is never split. |

Request bodies are stored as structured JSON and therefore remain sensitive
even when header capture is disabled. Each serialized file also has a hard
4 MiB limit.

On Unix, CCR-Rust enforces mode `0700` on the capture directory and mode `0600`
on newly created files. It refuses a symlink as the capture directory and uses
exclusive file creation so an existing path is never overwritten.

## What is captured

For non-streaming requests, CCR-Rust can record connection errors, HTTP error
responses, provider errors embedded in HTTP 200 bodies, and—only when
`capture_success` is true—successful responses.

For streaming requests, capture is deliberately limited to failures known
before the response stream is handed to the client: connection failures,
non-success HTTP responses, and an error detected in the initial bounded stream
peek. Successful stream bodies and errors that occur later in an active stream
are not persisted.

## Read captures locally

```bash
# Recent failures and metadata
ccr-rust captures --limit 10

# One provider
ccr-rust captures --provider minimax --limit 20

# Aggregate statistics
ccr-rust captures --stats

# Include the complete bounded response body in terminal output
ccr-rust captures --provider minimax --limit 5 --full
```

The CLI reads only regular files whose names begin with
`ccr_capture_v1_`. A list or statistics pass returns at most 100 valid captures,
reads no file larger than 4 MiB, and stops at 16 MiB of accepted input. Symlinks,
legacy files, task captures, and unrelated JSON are ignored.

Avoid redirecting `--full` output into a shared or world-readable location.

## File format

Files use this naming contract:

```text
ccr_capture_v1_{provider}_{tier_name}_{timestamp}_{request_id}.json
```

Example:

```text
ccr_capture_v1_minimax_ccr-mm_20260709_183742_123456789_42.json
```

Each JSON object contains request identity, provider, tier, model, timestamp,
URL, method, request body, response status and bounded body, latency, streaming
state, success state, and an optional error. Headers are absent unless
`include_headers` was explicitly enabled.

`success` is true only when the status is 2xx and no provider or transport
error was recorded.

## Rotation and cleanup

Retention runs after every successful write and deletes only the oldest
current-format regular files until `max_files` is satisfied. If retention
cannot enforce the configured bound, CCR-Rust removes the new file and reports
the error rather than increasing disk use.

Disable capture immediately after the bounded investigation:

```json
{
  "DebugCapture": {
    "enabled": false
  }
}
```

To remove only current-format captures:

```bash
find ~/.ccr-rust/captures -type f -name 'ccr_capture_v1_*.json' -delete
```

Legacy and unrelated files are intentionally left for an operator to review
and remove separately.
