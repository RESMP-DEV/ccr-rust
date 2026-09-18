# Claude Code through CCR

Keep Claude Code as the interface while using a provider configured in CCR.
Routing through an API-key provider uses that provider's plan; it does not
extend or transfer a Claude subscription. Select CCR explicitly for a session
and keep ordinary `claude` launches on their existing configuration.

On the configured workstation, run `claude-ccr`. It starts the local service
if needed and selects the configured Z.AI route. See
[Local operation](local-operation.md) for helper files, service commands, and
the dated live-client acceptance. Those helpers are not installed by Cargo.
For personal/corporate API plans or Claude logins, use
[Authentication suites](auth-suites.md).

## Prepare the router

Build and install CCR, then create a config with one provider you have access
to. See [Local operation](local-operation.md#set-up-another-machine) and
[Configuration](configuration.md). An Anthropic-compatible Z.AI coding-plan
endpoint can be configured as:

```json
{
  "Providers": [{
    "name": "zai",
    "api_base_url": "https://api.z.ai/api/anthropic/v1",
    "api_key": "${ZAI_API_KEY}",
    "protocol": "anthropic",
    "auth_header": "authorization",
    "models": ["glm-5.3"]
  }],
  "Router": {
    "default": "zai,glm-5.3",
    "tiers": ["zai,glm-5.3"]
  },
  "HOST": "127.0.0.1",
  "PORT": 3456
}
```

Verify the endpoint/model against your own plan. OpenAI-compatible providers
can also be used; CCR translates Claude's Anthropic requests for them. Add
provider-specific transformers only where the upstream needs them.

Load `ZAI_API_KEY` into the **router's** environment using your secret manager
or a private, trusted shell file. CCR does not load `.env` itself. Before
starting, check presence without printing the key:

```bash
: "${ZAI_API_KEY:?Load ZAI_API_KEY before starting CCR}"
ccr-rust --config "$HOME/.claude-code-router/config.json" validate
ccr-rust --config "$HOME/.claude-code-router/config.json" start --host 127.0.0.1 --port 3456
```

In another terminal, verify `curl --fail http://127.0.0.1:3456/health`.
Passing `validate` or health does not prove the provider accepts the key.

## Opt in for one Claude session

```bash
ANTHROPIC_BASE_URL=http://127.0.0.1:3456 \
ANTHROPIC_AUTH_TOKEN=ccr-local ANTHROPIC_API_KEY=ccr-local \
ANTHROPIC_MODEL=zai,glm-5.3 \
ANTHROPIC_DEFAULT_OPUS_MODEL=zai,glm-5.3 \
ANTHROPIC_DEFAULT_SONNET_MODEL=zai,glm-5.3 \
ANTHROPIC_DEFAULT_HAIKU_MODEL=zai,glm-5.3 claude
```

The default model overrides keep secondary Claude requests on the chosen
model. Replace every route above if using a different provider. These variables
apply only to this process; avoid global localhost overrides in shell startup
files or normal Claude settings.

The dummy client token satisfies client-side key requirements. CCR's HTTP
listener does not authenticate clients; it chooses upstream credentials from
its config. Keep it on loopback. A custom model name may produce an
`unrecognized_model` warning in Claude; verify the actual request and tool
behavior rather than treating the warning alone as failure.

If your normal setup uses Bedrock, Vertex, Foundry, or another host-managed
provider, inspect those client overrides too. This example assumes the ordinary
Anthropic-compatible transport and does not override organization policies.

## Verify a real tool round trip

Check `claude --version`, then use a disposable directory and the
[file-reading probe](local-operation.md#prove-the-selected-path). Confirm
Claude called `Read`, received the file contents, and returned the exact marker.
Compare CCR frontend and per-tier metrics before and after. A successful curl
or plain greeting is insufficient to claim Claude's tools work.

Claude Code 2.1.276 completed this probe with the example Z.AI route on
September 17, 2026. Repeat it when changing the CLI or provider.

## Fallback and account selection

Add providers to `Router.tiers` when fallback between them is intended. A 429
stops attempts for that tier and allows later eligible tiers; if every candidate
is rate-limited, CCR returns a 429.

An explicit `provider,model` selection already in the tier list retains other
fallback candidates. Keep personal and corporate accounts in separate
processes/configs if requests must not cross accounts. For native Claude
subscription login switching, use separate Claude homes as described in
[Authentication suites](auth-suites.md); CCR does not sign in to those plans.

## Troubleshooting

- Connection refused: verify service and client use the same host/port.
- Authentication failure: check the router environment and upstream endpoint;
  do not print provider keys or credential files.
- Unexpected provider: inspect tiers and model/search overrides, then check
  actual per-tier usage counters.
- Tool failure: inspect the client trace, model tool support, and translation
  path, including streaming and non-streaming where applicable.

See [Observability](observability.md), [CLI reference](cli.md), and
[Troubleshooting](troubleshooting.md) for more operational checks.
