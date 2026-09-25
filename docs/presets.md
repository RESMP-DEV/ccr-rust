# Named routing presets

Presets give **Anthropic Messages clients** a named route and optional parameter
overrides. For launch flags and Codex profiles, see [Switching plans](switching-plans.md).
A preset is not a credential store and does not isolate accounts.

Use a build containing the preset-route registration fix. Older builds
registered the newer `{name}` syntax against Axum 0.7, so listing presets worked
but `/preset/NAME/v1/messages` returned 404. Rebuild/install the updated source
and restart that router; the CLI version string alone does not identify the fix.

## Define presets

Use the top-level `Presets` object in your router config. The referenced
providers/models must also exist under `Providers`:

```json
{
  "Presets": {
    "glm": {"route": "zai,glm-5.3"},
    "kimi": {"route": "kimi,k3-256k", "max_tokens": 8192},
    "short": {"route": "zai,glm-5.3", "max_tokens": 256, "temperature": 0.2}
  }
}
```

A complete config is available in
[`examples/coding-plans.json`](../examples/coding-plans.json).

Only `route`, `max_tokens`, and `temperature` are implemented preset fields.
Arbitrary parameters and `extra_params` are not applied. Do not rely on the
similarly named `Router.presets` field; the HTTP handler reads the top-level
`Presets` collection. Presets are loaded from the file at router startup;
there are no automatically installed `coding`, `reasoning`, or `documentation`
presets. Define any such names explicitly.

## Request a preset

```bash
curl --fail --silent http://127.0.0.1:3456/preset/kimi/v1/messages \
  -H 'Content-Type: application/json' \
  -d '{"model":"placeholder","max_tokens":1024,"messages":[{"role":"user","content":"Say hello."}]}'
```

The request body must include a `model` string for parsing even though the
preset replaces it. The preset route always wins over the submitted model.
Configured preset `max_tokens` and `temperature` also win over request values;
unset preset values leave the request values alone. Provider transformers and
routing still run afterward.

`GET /v1/presets` returns an **array of objects**, with no guaranteed order:

```json
[
  {"name":"glm","route":"zai,glm-5.3","max_tokens":null,"temperature":null},
  {"name":"kimi","route":"kimi,k3-256k","max_tokens":8192,"temperature":null}
]
```

An unknown preset returns 404 when the request body is otherwise valid.
The only preset inference endpoint is `/preset/{name}/v1/messages`.
`/preset/{name}/v1/responses` and `/preset/{name}/v1/chat/completions` are not
registered; use Codex's model/profile selection instead. Do not assume a client
can use a preset base URL for its other API calls, such as listing models.

## Fallback and search routing

A preset delegates to ordinary routing after replacing the model. If that
route is in `Router.tiers`, the other candidates remain eligible for fallback.
`ignoreDirect` or search routing can also affect selection. Keep different
account suites in separate router instances when requests must not cross them.

Search tagging is a separate optional routing feature:

```json
{
  "Router": {
    "default": "zai,glm-5.3",
    "webSearch": {
      "enabled": true,
      "search_provider": "search-provider,search-model"
    }
  }
}
```

Use the exact outer key `webSearch`, and configure the referenced provider.
The current handler detects `[search]` or `[web]` in **string-valued** message
content, strips the tags, and prepends the search route to the candidates.
It does not scan structured content-block arrays for these tags. This routes
to a provider; it does not add web-search capability to a model that lacks it.
