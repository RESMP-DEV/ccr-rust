# Kimi Code setup

Use a **Kimi Code** API key from the [Kimi Code console](https://www.kimi.com/code/console).
A Moonshot/Kimi Platform pay-as-you-go key is a separate product; do not assume
it spends your Kimi Code membership quota. The environment name `KIMI_API_KEY`
is just the name used by these examples, not a conversion between key types.

For launch commands, see [Switching coding plans](switching-plans.md). Keep
personal/corporate credentials in [separate suites](auth-suites.md).

## Endpoint and model selection

| Use | Base URL | Request endpoint |
| --- | --- | --- |
| CCR Anthropic provider | `https://api.kimi.com/coding/v1` | CCR appends `/messages` |
| Direct Claude/Anthropic SDK | `https://api.kimi.com/coding/` | Client appends `/v1/messages` |
| OpenAI Chat Completions | `https://api.kimi.com/coding/v1` | `/chat/completions` |

The current [official model catalog](https://www.kimi.com/code/docs/en/) lists
`kimi-for-coding`, `kimi-for-coding-highspeed`, `k3`, and `k3-256k`. Access and
context limits depend on membership. `kimi-for-coding` is a service alias;
it is not a pinned model release. Use the account's catalog and a real request
rather than assuming the older `kimi-k2.5` or `kimi-k2-thinking` examples apply
to the coding-plan endpoint.

## CCR configuration for Claude and Codex

This is a complete single-provider config:

```json
{
  "Providers": [{
    "name": "kimi",
    "api_base_url": "https://api.kimi.com/coding/v1",
    "api_key": "${KIMI_API_KEY}",
    "protocol": "anthropic",
    "models": ["kimi-for-coding", "k3-256k"],
    "transformer": {
      "use": [["maxtoken", {"max_tokens": 8192}]]
    }
  }],
  "Router": {
    "default": "kimi,k3-256k",
    "tiers": ["kimi,k3-256k"],
    "ignoreDirect": false
  },
  "HOST": "127.0.0.1",
  "PORT": 3456
}
```

The Anthropic protocol uses `x-api-key` by default. No fabricated client
identity or special `User-Agent` is needed by the verified configuration.

**Keep the `maxtoken` transformer for Codex.** The tested Codex build omitted
an output limit; CCR forwarded the Anthropic request without `max_tokens`, and
Kimi returned HTTP 400. The existing transformer adds the missing limit. In
the router's active registry it also caps a larger requested limit while
preserving a smaller one. `8192` is a starter output budget, including reasoning
where counted by the upstream; adjust it to the task and provider's limits if
long answers or large edits are truncated.

Do not add `kimi` or format-conversion transformers solely because of the
provider name. The native Anthropic path already preserves its content blocks.
The historical `kimi` transformer handles a different response convention;
it is not a login mechanism or a substitute for required request fields.

Load the key into the router process, then validate and start your saved file:

```bash
: "${KIMI_API_KEY:?Load your Kimi Code key}"
ccr-rust --config /path/to/kimi.json validate
ccr-rust --config /path/to/kimi.json start --host 127.0.0.1 --port 3456
```

For both providers in one authorized suite, use
[`examples/coding-plans.json`](../examples/coding-plans.json). There is no
built-in `ccr-kimi` command. Use the `claude_route` recipe or an explicit Codex
route from [Switching plans](switching-plans.md).

## Verify and diagnose

- `/v1/models` on CCR lists configuration. It does not authenticate upstream.
- HTTP 401: verify that the key belongs to Kimi Code and that the running router
  loaded the intended credential file; do not print the key.
- HTTP 400 from the Anthropic endpoint with Codex: check that `max_tokens` is
  present after the transformer chain. A passing `validate` does not prove this.
- Tool-name errors on the OpenAI endpoint: do not assume native Responses tools
  are valid Chat Completions tools. The configuration above uses the verified
  Anthropic translation path.
- 429 or transport failure: keep CCR's normal retry/rate-limit policy. A key or
  model change should not silently switch to a different account.

Run a file-reading tool round trip with the actual installed client and verify
both the tool result and per-tier counters. Context sizes, supported tools,
reasoning controls, and model metadata are separate from selecting a route.

## Verified September 17, 2026

Using the CCR release built from `ee0fec7` (application code unchanged through
`66ad937`), Claude Code 2.1.276 and the local Codex 0.0.0 build each read a random
file through both `k3-256k` and `kimi-for-coding` and returned its exact marker.
Codex required the `maxtoken` configuration above. One alias run had a transport
failure after its tool call; repeating with the ordinary retry policy passed.

The available Kimi Code credential returned HTTP 200 and the four model IDs
listed above. An older credential source returned 401; file presence alone was
not evidence of a working plan. No new key was created or rotated. These tests
used temporary router instances and synthetic file content, not private project
work. Other model IDs and membership tiers were not tested for inference.

References checked on that date: [Kimi Code overview](https://www.kimi.com/code/docs/en/)
and [Claude Code integration](https://www.kimi.com/code/docs/en/third-party-tools/claude-code).
