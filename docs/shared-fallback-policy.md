# Shared fallback policy

Several CCR listeners can share one fallback policy while retaining their own
primary model, protocol, client aliases and retry hold limits. The optional
Python helper in `scripts/ccr_fallback_policy.py` derives ordinary CCR JSON
configuration files. It is a local operator tool, not a new router protocol or
a command installed by Cargo. Python 3.10 or newer is required.

Use one policy only within an authorized account suite. Keep personal and
corporate suites separate as described in [Authentication suites](auth-suites.md).
The policy deliberately authorizes requests to cross its provider tiers.

## Define and synchronize

Keep `fallback-policy.json` beside the existing listener configurations. For
example, with `zai` already defined in both consumers and advertising the three
GLM model IDs below:

```json
{
  "version": 1,
  "routes": [
    "zai,glm-5.3-flashx",
    "zai,glm-5.3-flash",
    "zai,glm-5.3",
    "kimi,k3-256k"
  ],
  "providers": [{
    "name": "kimi",
    "api_base_url": "https://api.kimi.com/coding/v1",
    "api_key": "${CCR_KIMI_API_KEY}",
    "protocol": "anthropic",
    "models": ["k3-256k"],
    "transformer": {"use": [["maxtoken", {"max_tokens": 32768}]]}
  }],
  "consumers": {
    "glm-workers.json": {"primary": "zai,glm-5.3-flashx"},
    "config.json": {"primary": "zai,glm-5.3"}
  }
}
```

The consumer's primary goes first, followed by the shared chain with duplicates
removed. The final shared route stays last. An optional `additional_routes`
list per consumer inserts its existing extra providers before that final route;
those providers must be defined in that consumer. Shared provider definitions
replace providers with the same name. Other provider definitions remain local,
so one listener can use native Responses while another uses Anthropic for Z.ai.

Each consumer must already enable `Router.retrySweeps`. The helper preserves its
cooldown and hold limits and sets `strictTierOrder: true` and `ignoreDirect: false`.
Remove any `Router.topK` setting first; CCR rejects it with strict ordering.
CCR still promotes an explicitly requested tier to the front. Explicit routes
outside the tier list retain CCR's single-provider behavior. The final fallback
is therefore last for normal primary requests, not a restriction on an explicit
user selection of that model. There is no new retry loop in the helper: CCR's
existing per-tier backoff, Retry-After handling and retry sweeps remain in charge.

From the source checkout:

```bash
uv run --no-project python scripts/ccr_fallback_policy.py check --json
uv run --no-project python scripts/ccr_fallback_policy.py sync --json
```

Use `--root /path/to/suite` for another configuration directory. `check` exits
0 for matching files, 1 for drift and 2 for invalid policy or an I/O error.
`sync` validates all registered consumers before writing and replaces each
changed file atomically. Updates across multiple files are not a transaction;
if an I/O failure interrupts a sync, run `check` and sync again before restart.
The reported hash identifies the policy, not the state of a running listener.

Back up existing configurations before the first sync. Keep credential values
in private runtime storage, never in the policy. Shared providers require an
environment-variable reference. The catalog and config only establish route
availability in configuration; verify account access with a live request.

## Reuse at startup

Install a single copy of the module and import it from each service launcher.
For example, after making that installed directory importable:

```python
from ccr_fallback_policy import ROOT, serve

serve(ROOT / "glm-workers.json", ["start", "--host", "127.0.0.1", "--port", "3457"])
```

`serve` derives that registered consumer at startup, reads its referenced
variables from `runtime-credentials.json` in the policy directory, then executes
`~/.cargo/bin/ccr-rust`. The credential file must be private (mode 0600).
Explicit inherited environment variables can supply references absent from the
file. Other client settings and filesystem/network permissions are unchanged.
Worker launchers can use `load_policy` and `derive` to validate approved routes
instead of maintaining their own allowed-model list. A requested model in a
worker receipt is not proof of the upstream used; inspect actual router logs or
per-tier counters.

Sync edits files only. CCR loads configuration at startup, so inspect
`ccr_active_requests` and coordinate an idle restart for each affected listener.
Do not interrupt active work to make an updated file appear live.

## Verification

```bash
uv run --no-project python -m unittest discover -s scripts -p 'test_ccr_fallback_*.py'
```

The policy tests cover cross-consumer updates, primary and limit retention,
invalid configurations and private credential loading. Runtime tests require
`ccr-rust` on PATH (or `CCR_TEST_BINARY`) and exercise the installed binary against
local Responses and Anthropic fixtures: primary success, each successive
fallback, preserved max reasoning requests, and all-tier rate limiting with
backoff. These are deterministic protocol checks, not live model quality tests.
Qualify a real client tool round trip separately after adding a provider.
