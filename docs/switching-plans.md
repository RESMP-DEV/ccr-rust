# Switch coding plans and models

Choose the **account suite**, then the **provider/model**, then the client.
Switching from Kimi to GLM changes the upstream plan used for that request;
changing between personal and corporate credentials needs the separate stores
and processes in [Authentication suites](auth-suites.md).

## Choose a switching method

| Method | Best use | Scope |
| --- | --- | --- |
| Launch with a `provider,model` route | Choose Kimi or GLM for a new session | Client process; other router tiers may still be fallback candidates |
| Save an opt-in Codex profile | Repeat the same provider/model choice | Named profile; shares login storage unless using a separate home |
| Claude's `/model` command | Change the main conversation model in an existing session | Does not rewrite all background/subagent model defaults |
| Anthropic preset URL | Give an HTTP client a named route and parameter defaults | `/preset/NAME/v1/messages` only |
| Change `Router.default` / `Router.tiers` | Change default routing or enable deliberate automatic fallback | All clients using that router after restart |
| Separate config/process/port | Switch account, credentials, or permitted provider set | Whole suite; recommended for personal/corporate separation |
| Direct provider connection | Use the provider without CCR translation | Client-specific endpoint and credential config |

A route name is **`provider,model`**, with a comma, not a slash. A bare model ID
such as `k3-256k` does not by itself select the `kimi` provider: CCR normally
uses its router defaults for requests without an explicit provider prefix.
`/v1/models` lists configured routes, not account entitlement.

## Configure Kimi and GLM once

[`examples/coding-plans.json`](../examples/coding-plans.json) contains both
providers and two named presets. Use only credentials belonging to the same
authorized suite in this combined example. Remove providers/models you do not
have access to. The Kimi `maxtoken` transformer supplies the output limit
required by its Anthropic endpoint when Codex omits one; see [Kimi setup](kimi_setup.md).

Load `KIMI_API_KEY` and `ZAI_API_KEY` into the router process from private storage,
then run from the checkout in a separate terminal:

```bash
: "${KIMI_API_KEY:?Load this suites Kimi Code key}"
: "${ZAI_API_KEY:?Load this suites Z.AI plan key}"
ccr-rust --config examples/coding-plans.json validate
ccr-rust --config examples/coding-plans.json start --host 127.0.0.1 --port 3456
```

Stop an existing foreground router first or choose an unused port and update
the client URLs below. For the managed workstation service, merge provider
entries into its active config and extend its credential loader explicitly;
editing an old `.env` does not update the service. Back up those files first.
See [Local operation](local-operation.md). These instructions do not change
normal `claude` or `codex` launches.

The example defaults to GLM with only GLM in `Router.tiers`. An explicit Kimi
route outside that list becomes a single candidate. This gives intentional
provider selection for this example, but is not access control. For accounts
that must never mix, keep their provider sets in different router instances.

## Claude Code: select a plan for a new session

This shell function is a recipe, not a built-in CCR command. It sets the main,
background, and subagent model choices together:

```bash
claude_route() {
  local route="${1:?Use provider,model}"
  shift
  ANTHROPIC_BASE_URL="${CCR_BASE_URL:-http://127.0.0.1:3456}" \
  ANTHROPIC_AUTH_TOKEN=ccr-local ANTHROPIC_API_KEY=ccr-local \
  ANTHROPIC_MODEL="$route" \
  ANTHROPIC_DEFAULT_FABLE_MODEL="$route" \
  ANTHROPIC_DEFAULT_OPUS_MODEL="$route" \
  ANTHROPIC_DEFAULT_SONNET_MODEL="$route" \
  ANTHROPIC_DEFAULT_HAIKU_MODEL="$route" \
  CLAUDE_CODE_SUBAGENT_MODEL="$route" \
    claude --model "$route" "$@"
}

claude_route kimi,k3-256k
claude_route kimi,kimi-for-coding
claude_route zai,glm-5.3
# Another account suite, if that instance has this route:
CCR_BASE_URL=http://127.0.0.1:3459 claude_route zai,glm-5.3
```

The current workstation's `claude-ccr` helper fixes its default model variables
to GLM. Passing only `claude-ccr --model kimi,k3-256k` changes the main model
while its other defaults can still name GLM. Use the function above or separate
plan-specific launchers when the entire session should stay with one provider.

Claude also supports `/model` during a session. For example,
`/model kimi,k3-256k` changes the main conversation model where custom names are
accepted by the installed version. Start a fresh session with all overrides
when changing accounts or when background requests must follow the same plan.
Check `/status`, settings-file overrides, and actual CCR per-tier counters.

## Codex: select a route or save a profile

After creating the CCR profile from [Codex setup](codex_setup.md):

```bash
codex --profile ccr --model kimi,k3-256k
codex --profile ccr --model zai,glm-5.3
# On the workstation, these also start the managed service if stopped:
codex-ccr --model kimi,k3-256k
codex-ccr --model zai,glm-5.3
```

The chosen route must exist in that instance. A working provider credential in
another file is not enough. Custom route names may produce missing model
metadata warnings; those warnings are separate from upstream authentication or
tool compatibility errors.

For the local fork that loads `<name>.config.toml`, copy the working
`~/.codex/ccr.config.toml` to a **new** `ccr-kimi.config.toml` and set its top-level
`model = "kimi,k3-256k"`. Make another profile named `ccr-glm.config.toml` with
`model = "zai,glm-5.3"`. Keep the provider URL and HTTP/SSE settings. Then:

```bash
codex --profile ccr-kimi
codex --profile ccr-glm
```

For builds using inline `[profiles.NAME]`, add the corresponding named profile
tables to the existing config instead. Do not set a global default profile.
A profile selects configuration; use a separate `CODEX_HOME` for a different
native login store.

## Presets, defaults, and automatic fallback

The example's named presets are available to Anthropic HTTP clients:

```bash
curl --fail --silent http://127.0.0.1:3456/preset/kimi/v1/messages \
  -H 'Content-Type: application/json' \
  -d '{"model":"unused-by-preset","max_tokens":1024,"messages":[{"role":"user","content":"Say hello."}]}'
curl --fail --silent http://127.0.0.1:3456/v1/presets
```

The body still needs a `model` string for parsing; the preset replaces it.
This URL requires the preset-registration fix described in [Presets](presets.md);
older binaries return 404 even when the preset appears in the listing.
Preset `max_tokens` and `temperature`, when configured, **override** request
values. There is no preset `/v1/responses` route for Codex, and setting a
`Presets` entry does not change the default route. See [Presets](presets.md).

To make Kimi the router default, change both `Router.default` and the explicit
`Router.tiers` list to `kimi,k3-256k`, then restart. To allow automatic fallback,
deliberately list both routes in `Router.tiers`. EWMA/GP may influence ordering;
a direct route already in the list is prioritized but retains the other
candidates. This can spend quota on another plan. A preset pin has the same
fallback semantics.

## Direct provider connections

CCR is optional. Claude can connect directly using Kimi's Anthropic base
`https://api.kimi.com/coding/` or Z.AI's
`https://api.z.ai/api/anthropic`. Direct connections need the real provider key
in the client process, rather than the dummy localhost token, and bare upstream
model names rather than `provider,model` route names.

For direct Codex-to-Z.AI use, Z.AI now documents the Responses base
`https://api.z.ai/api/v1`. Configure it in an opt-in provider profile with
`wire_api = "responses"`, `supports_websockets = false`, and an environment-backed
`env_key`; keep the key out of TOML. See the [Z.AI guide](zai_minimax_setup.md)
for endpoint differences. Kimi's advertised OpenAI compatibility is Chat
Completions, so it does not by itself establish native Codex Responses support.

The official Kimi client also has its own login and model selection. Check
`kimi --version` and `kimi --help` against the current
[Kimi Code guide](https://www.kimi.com/code/docs/en/); the locally installed
Python-era 1.49.0 CLI differs from the current documented Node-based client.
Its login is not automatically imported into CCR.

## Verify after switching

Compare `/v1/usage` and `/v1/frontend-metrics` before and after a request, then
run a harmless file-reading tool round trip as described in
[Local operation](local-operation.md#prove-the-selected-path). Check the actual
tier and client tool result, not just the model label. Changing account or
provider inside an existing conversation may send previous context to the new
provider; start a fresh session when that context should stay separate.

The dated provider/client results and known configuration requirements are in
[Kimi setup](kimi_setup.md) and [Z.AI/MiniMax setup](zai_minimax_setup.md).
