# Presets

Named configurations for quick routing.

## Configuration

```json
{
  "presets": {
    "fast": {"route": "groq,llama-3", "max_tokens": 2048},
    "smart": {"route": "anthropic,claude-3-opus"}
  }
}
```

## Usage

```bash
curl http://localhost:3456/preset/fast/v1/messages \
  -H "Content-Type: application/json" \
  -d '{"messages": [{"role": "user", "content": "Hello"}]}'
```

## Listing Presets

```bash
curl http://localhost:3456/v1/presets
```
