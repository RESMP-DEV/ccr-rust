# Z.AI Anthropic endpoint: empirical contract

Everything below was verified live against `https://api.z.ai/api/anthropic/v1`
on 2026-09-23 through CCR-Rust, probing with generated PNGs (solid colors and
a structured red/blue split image). Where official Z.AI documentation and
observed behavior could differ, this file records observed behavior.

## Endpoint and authentication

- Base URL: `https://api.z.ai/api/anthropic/v1`, messages route
  `/v1/messages` (CCR appends `/messages` to the base).
- Auth: `Authorization: Bearer <key>` (configure `"auth_header":
  "authorization"` on the provider; CCR's default is `x-api-key`, which this
  endpoint does not accept).
- 401 body: `{"error":{"message":"token expired or incorrect","type":"401"}}`.
- The OpenAI-compatible sibling `https://api.z.ai/api/v1` (chat completions)
  takes the same key with `Authorization: Bearer`.

Provider block used locally (`~/.claude-code-router/config.json`):

```json
{
  "name": "zai",
  "api_base_url": "https://api.z.ai/api/anthropic/v1",
  "api_key": "${CCR_ZAI_API_KEY}",
  "protocol": "anthropic",
  "auth_header": "authorization",
  "models": ["glm-5.3"],
  "honor_ratelimit_headers": false
}
```

## Content blocks

- Native Anthropic blocks (`text`, `image`, `tool_use`, `tool_result`,
  `thinking`) are accepted as-is. CCR passes `messages[].content` through
  verbatim when no normalization is needed.
- **OpenAI-style blocks are silently ignored.** A `{"type":"image_url",...}`
  block in message content does not error; the model just never sees the
  image. CCR-Rust converts `image_url` to Anthropic `image` blocks at its
  internal boundary (commit `24260ee`), so Responses/chat clients are
  protected from this, but raw API callers must send native blocks.
- Images: both `{"type":"base64","media_type":...,"data":...}` and
  `{"type":"url","url":...}` sources are processed. A 64x64 PNG added roughly
  150-160 input tokens in probes.

## Streaming

Standard Anthropic SSE: `message_start`, `ping`, `content_block_start`
(`thinking` blocks carry `signature` and stream via `thinking_delta`; text
via `text_delta`), `content_block_stop`, `message_delta`, `message_stop`.
Reasoning models emit `thinking` blocks first; clients that discard them
still receive the final text.

## Model behavior observed

- **Vision policy: use glm-5.3 flash (flashx) for image workloads, never
  glm-5.3 standard.** Flash has native (offloaded) vision and read structured
  probes correctly through the OpenAI-compatible endpoint. glm-5.3 standard
  segments image layout reliably (it answers with the correct left/right
  structure) but misreads colors: solid blue read as "green"/"purple", red as
  "blue", across both zai endpoint styles. Treat standard's image answers as
  unreliable regardless of transport.
- Solid single-color tiny images (1x1) are misclassified even by otherwise
  accurate models; use structured probes when verifying image transport.

## Verifying image transport (reusable probes)

Generate a two-color PNG, send it as an Anthropic image block, and expect
`left=red, right=blue`:

```bash
python3 - <<'EOF'
import zlib, struct, base64, json
def chunk(t, d):
    c = t + d
    return struct.pack('>I', len(d)) + c + struct.pack('>I', zlib.crc32(c) & 0xffffffff)
w, h = 256, 128
raw = b''.join(b'\x00' + (bytes((255,0,0))* (w//2) + bytes((0,0,255))*(w//2)) for _ in range(h))
png = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)) \
    + chunk(b'IDAT', zlib.compress(raw)) + chunk(b'IEND', b'')
body = {"model":"glm-5.3","max_tokens":1024,"stream":False,"messages":[{"role":"user","content":[
    {"type":"text","text":"Two colored halves. Reply exactly as left=<color>, right=<color>"},
    {"type":"image","source":{"type":"base64","media_type":"image/png","data":base64.b64encode(png).decode()}}]}]}
open('/tmp/image-probe.json','w').write(json.dumps(body))
EOF
curl -s http://127.0.0.1:3456/v1/messages -H 'content-type: application/json' \
  -H 'anthropic-version: 2023-06-01' --data-binary @/tmp/image-probe.json
```

Input-token inflation (roughly +150 tokens versus the text-only prompt)
confirms the image was processed; a model claiming "no image attached" means
transport dropped it.

## Operations: restart with credentials (do not bypass)

The local configs reference `api_key: "${CCR_ZAI_API_KEY}"` /
`${CCR_AZURE_API_KEY}`. Those variables are injected only by
`~/.claude-code-router/serve.py`, which reads `runtime-credentials.json`
(mode 0600) and `execve`s the router. **Starting `ccr-rust` directly from a
shell without those variables makes both providers 401** with the literal
placeholder string as the key; the failure looks exactly like an expired
credential. This happened on 2026-09-23 after a manual restart and was
misdiagnosed as key expiry before the env loss was found.

Canonical restarts on this workstation are **launchd**, not manual shells:
`com.kearm.ccr-rust` (port 3456, config.json) and `com.kearm.ccr-glm-workers`
(port 3457, glm-workers.json via `CCR_CONFIG_FILE`) both run `serve.py` with
`KeepAlive`, so a manually started listener fights the agent (recurring
"Address already in use") and the agent reclaims the port within seconds of
any manual process exiting.

```bash
launchctl kickstart -k gui/$(id -u)/com.kearm.ccr-rust
launchctl kickstart -k gui/$(id -u)/com.kearm.ccr-glm-workers
```

Service logs land in `~/.claude-code-router/logs/service.stdout.log` and
`service.stderr.log`, not the manual `ccr-34xx.log` files. `serve.py` reads
`runtime-credentials.json` (mode 0600; entries limited to `CCR_ZAI_API_KEY`,
`CCR_AZURE_API_KEY`, `CCR_MINIMAX_API_KEY`) and injects them before exec, so
launchd-spawned processes always carry credentials. If you must start
manually, use `serve.py` the same way — never raw `ccr-rust start`.

Check for in-flight `codex exec` workers before restarting the 3457 service;
drain first. Verify after restart: `curl -s :PORT/health`, one cheap
completion per provider, and `ps eww <pid> | tr ' ' '\n' | grep '^CCR_'`
must show the injected variables.

CCR-Rust also refuses to start (`unexpanded credential references: ...`) when
a provider `api_key` or `extra_headers` value still contains a `${VAR}`
placeholder after environment expansion, so a raw `ccr-rust start` without
the launcher fails immediately instead of serving placeholder-key 401s.
`CCR_ALLOW_UNEXPANDED_CREDENTIALS=true` overrides for intentionally keyless
setups, and any 401 that still occurs with a placeholder key carries a
restart hint in the router log.

## Related

- [Debug capture](debug_capture.md) for wire-level request/response records.
- [Configuration](configuration.md) for provider schema and env expansion.
- Image routing matrix and the conversion fix: commit `24260ee`; request
  body limits: commit `e106d12` and `MAX_REQUEST_BODY_BYTES`.
