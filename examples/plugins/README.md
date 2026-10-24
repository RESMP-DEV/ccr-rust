# Example transformer plugins

Drop-in plugins for the CCR-Rust `Plugins` system. Each file here is a
self-contained executable that reads one JSON document (request or response)
on stdin and writes the transformed JSON document on stdout. See
[docs/plugins.md](../../docs/plugins.md) for the full protocol.

To install one, copy it into the plugin directory and make it executable:

```sh
mkdir -p ~/.claude-code-router/plugins
cp strip-cache-control.sh ~/.claude-code-router/plugins/
chmod +x ~/.claude-code-router/plugins/strip-cache-control.sh
ccr-rust plugins            # should list strip-cache-control (source: directory)
```

Then reference it from a provider's transformer chain:

```json
{
  "transformer": {
    "use": ["strip-cache-control"]
  }
}
```

All examples require `jq` (Homebrew: `brew install jq`).

| Plugin | Stages | What it does |
|--------|--------|--------------|
| `strip-cache-control.sh` | both | Removes Anthropic `cache_control` markers from every block. Useful for OpenAI-compatible upstreams that reject unknown fields on otherwise-valid payloads. |
| `log-payload.sh` | both | Appends one JSON line (`stage`, `plugin`, `payload`) to `$CCR_PLUGIN_LOG` (default `/tmp/ccr-plugin.log`), then forwards the payload unchanged. Use it to see exactly what the router sends and receives; remove it once diagnosed. |
