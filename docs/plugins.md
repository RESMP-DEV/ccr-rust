# Transformer plugins

Plugins add request/response transformers to CCR-Rust at runtime, without
recompiling the router. They restore the Node.js router's plugin
extensibility on the Rust router: any executable program that reads one JSON
document on stdin and writes the transformed JSON document on stdout can be
wired into any provider's transformer chain.

- Built-in transformers are compiled into the binary (`ccr-rust plugins`
  lists them with source `builtin`).
- Config plugins are declared in the `Plugins` section of `config.json`.
- Drop-in plugins are executable files in the plugin directory, named after
  their file stem.

The same resolution registers plugins at server start and drives the
`ccr-rust plugins` listing, so what the CLI shows is what the router runs.

## Wire protocol

The router spawns the plugin once per stage invocation, directly (never
through a shell):

- **stdin**: the JSON payload, one document. The request payload for the
  request stage, the response payload for the response stage.
- **stdout**: the transformed JSON payload, one document. Surrounding
  whitespace is fine; empty output is an error.
- **exit code**: `0` means success. Anything else (including a signal) is a
  failure.
- **stderr**: ignored on success, captured (last 4 KiB) on failure for the
  warning or error message.

Two environment variables identify the invocation, on top of the inherited
process environment plus any per-plugin `env` entries:

| Variable | Value |
|----------|-------|
| `CCR_STAGE` | `request` or `response` |
| `CCR_PLUGIN_NAME` | the plugin's registered name |

The plugin runs with the same timeout budget regardless of stage. When the
budget expires the router kills the plugin's entire process group, so shell
plugins that fork children cannot outlive their budget.

Minimal example plugin (`uppercase-demo.sh`):

```sh
#!/bin/sh
# Read JSON on stdin, write transformed JSON on stdout.
tr 'a-z' 'A-Z'
```

Ready-to-use examples live in [examples/plugins](../examples/plugins/).

## Configuration

```json
{
  "Plugins": {
    "dir": "~/.claude-code-router/plugins",
    "plugins": {
      "redact-pii": {
        "command": "/usr/local/bin/redact-pii",
        "args": ["--mode", "aggressive"],
        "timeout_ms": 2000,
        "env": { "REDACT_LOG": "/tmp/redact.log" },
        "stages": "both",
        "on_error": "fail"
      }
    }
  }
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `Plugins.dir` | string | `~/.claude-code-router/plugins` | Directory scanned for drop-in plugins. A missing directory is not an error. |
| `Plugins.plugins` | object | `{}` | Named plugin declarations keyed by the name used in transformer `use` lists. |
| `command` | string | required | Program to run. Bare names resolve through `PATH`; paths are used as-is. Never executed through a shell. |
| `args` | array of strings | `[]` | Static arguments passed to every invocation. |
| `timeout_ms` | number | `10000` | Kill the plugin (whole process group) after this many milliseconds. |
| `env` | object | `{}` | Extra environment variables layered on top of the inherited environment. |
| `stages` | string | `"both"` | `"request"`, `"response"`, or `"both"`. A stage outside this set passes through without spawning the plugin. |
| `on_error` | string | `"passthrough"` | `"passthrough"` logs a warning and passes the payload through unchanged; `"fail"` rejects the request. |

### Drop-in plugins

Every executable regular file in `Plugins.dir` becomes a plugin named after
its file stem: `redact-pii.sh` registers as `redact-pii`. Hidden files
(leading `.`), backups (`.bak`, `.tmp`, `.swp`, trailing `~`), and
non-executable files are skipped. Two files with the same stem collapse to
one plugin, keeping the lexicographically first path. Drop-ins use the
default timeout, both stages, and passthrough error handling; declare the
plugin in `Plugins.plugins` (same name) when you need different settings.

### Precedence

1. Built-in transformer names always win: a plugin named like a built-in
   (`anthropic`, `maxtoken`, ...) is skipped with a warning.
2. Config declarations override same-named drop-in files.
3. Everything else registers under its declared or derived name.

Plugins are referenced from a provider's transformer chain exactly like
built-ins:

```json
{
  "transformer": {
    "use": ["deepseek", ["maxtoken", {"max_tokens": 65536}], "redact-pii"]
  }
}
```

## Failure semantics

| Failure | `on_error: "passthrough"` (default) | `on_error: "fail"` |
|---------|--------------------------------------|---------------------|
| Spawn error (missing binary) | warning, payload unchanged | request rejected |
| Non-zero exit | warning, payload unchanged | request rejected |
| Timeout | warning after process-group kill, payload unchanged | request rejected |
| Invalid or empty stdout | warning, payload unchanged | request rejected |
| Stdout never closes (orphaned child holds the pipe) | warning after the 2 s drain grace, payload unchanged | request rejected |

Passthrough is the default because an external process is the fragile layer
and silently skipping an optional transform is less harmful than failing
every request on a tier. Prefer `on_error: "fail"` for correctness-critical
plugins such as PII redaction, where passing untransformed data through is
worse than failing.

Two call paths apply transformer errors differently, matching built-in
transformer behavior:

- The non-streaming request path propagates a `fail` transformer error as a
  failed request attempt.
- The streaming response path drops a failed response transform silently
  (`if let Ok(transformed)`), so a `fail` plugin cannot tear down a stream
  mid-flight; the warning is still logged.

## Validation and inspection

`Config::from_file` validates every declared plugin (name, command
resolution, positive timeout) before any listener starts, so `start`,
`validate`, and `doctor` all fail fast on a broken declaration.

```sh
ccr-rust plugins                 # human-readable table
ccr-rust plugins --json          # machine-readable
ccr-rust --config other.json plugins
```

## Limitations

- Plugins run per invocation; a shell startup or interpreter boot costs
  10-50 ms per stage on typical payloads. Hot paths with strict latency
  budgets should stay on built-in transformers.
- There is no streaming hook: plugins see the aggregated request/response
  document, exactly like built-in non-streaming transformers.
- Plugins are trusted operator configuration. They run with the router's
  user and environment; do not point them at untrusted executables.
