# Codex CLI through CCR

Select CCR explicitly for a session. Keep ordinary `codex` launches on their
existing provider. On the configured workstation, use `codex-ccr`; see
[Local operation](local-operation.md) for service commands and the dated live
client test. For personal/corporate accounts, use [Authentication suites](auth-suites.md).
For Kimi/GLM route selection, named profiles, and provider-specific requirements,
see [Switching plans](switching-plans.md#codex-select-a-route-or-save-a-profile).

The September 17, 2026 live tool-loop test used the local Codex development
build `0.0.0` with a Responses upstream. Other versions/providers need their
own client test. Check `codex --version` and `codex --help` first.

## Prepare the router

Configure a provider using [Configuration](configuration.md), load its API key
into the router process, and start it on loopback. Then check:

```bash
curl --fail --silent http://127.0.0.1:3456/health
curl --fail --silent http://127.0.0.1:3456/v1/models
```

The listed `provider,model` IDs are configured routes, not proof that the
account can access the upstream model. No upstream secret belongs in Codex's
CCR profile. CCR makes upstream requests using its own credentials.

## Configure an opt-in profile

Profile formats vary by build. Use the format your installed client supports;
do not replace an existing config file wholesale.

### Installed local fork: separate profile file

The locally tested build reads `~/.codex/ccr.config.toml` for `--profile ccr`:

```toml
model_provider = "ccr_local"
model = "gpt-6-astra"

[model_providers.ccr_local]
name = "CCR-Rust local"
base_url = "http://127.0.0.1:3456/v1"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
experimental_bearer_token = "ccr-local"
request_max_retries = 0
stream_max_retries = 0
stream_idle_timeout_ms = 180000
```

`gpt-6-astra` matches the model behind this workstation's CCR default route.
Keeping the known native model name lets Codex retain its model metadata;
CCR selects the backend using its routing config. Use `--model provider,model`
for another configured route. Custom names may produce missing-metadata warnings.

Do not combine this fork's `--ignore-user-config` with a named profile: it skips
the profile too. If testing without external MCPs, disable only those MCPs
explicitly instead of discarding the provider config.

### Builds with inline profiles

For versions whose schema uses `[profiles.NAME]`, merge this into
`~/.codex/config.toml` instead. Replace the model with a configured route.
Do **not** add a top-level `profile = "ccr"` default.

```toml
[profiles.ccr]
model_provider = "ccr_local"
model = "zai,glm-5.3"

[model_providers.ccr_local]
name = "CCR-Rust local"
base_url = "http://127.0.0.1:3456/v1"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
experimental_bearer_token = "ccr-local"
request_max_retries = 0
stream_max_retries = 0
stream_idle_timeout_ms = 180000
```

The client token is a dummy value, not authentication on CCR's listener.
`wire_api = "responses"` selects the Codex-to-CCR protocol; the upstream protocol
is configured independently in CCR. Keep `supports_websockets = false` for
HTTP/SSE. Initial zero client retries make router failures easier to diagnose;
CCR still applies its own retry policy.

## Launch and verify

```bash
codex --profile ccr
codex --profile ccr exec "Say hello in one sentence."
# Only if this route is configured:
codex --profile ccr --model zai,glm-5.3 exec "Say hello in one sentence."
```

A model/provider profile does not create a separate account store. If the chosen
route is in CCR's tier list, other tiers remain fallback candidates. Use
separate CCR instances for accounts that must not mix.

Compare `/v1/frontend-metrics` and `/v1/usage` before and after the request.
Then run the [file-reading tool probe](local-operation.md#prove-the-selected-path):
confirm an actual shell/tool call and exact file contents in the client trace.
Health or a Chat Completions curl alone does not test Codex's Responses
transport or tools. Keep normal client permissions; the old `--full` example
is not a portable Codex flag.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| Connection refused | Confirm service and profile use the same loopback port |
| Request reaches OpenAI instead of CCR | Inspect effective profile loading, provider selection, and `--ignore-user-config` |
| Upstream HTTP 401 | Check router credential presence and provider endpoint; `validate` does not authenticate |
| Model not found | Compare configured route with the upstream account's model/deployment access |
| Tools fail or disappear | Inspect the client trace and upstream tool support; test a full multi-turn exchange |
| Timeouts | Inspect router latency/failure counters and align client stream and router API timeouts |

Never debug authentication by printing `OPENAI_API_KEY` or dumping `auth.json`.
Check presence without printing values, and redact logs before sharing them.

CCR normalizes provider reasoning during protocol translation; reasoning and
tool-result continuity still depend on the upstream. See
[Codex API research](codex_api_research.md), [Streaming design](streaming_incremental_design.md),
and the `integration_codex*` / `integration_responses*` tests for implementation
coverage. Provider-specific transformers belong in the
[router configuration](configuration.md), not in a Codex login profile.
