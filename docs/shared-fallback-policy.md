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
example, with `zai` and `minimax` already defined in both consumers and
advertising the GLM model ID below:

```json
{
  "version": 1,
  "routes": [
    "minimax,MiniMax-M3.1-Flash",
    "zai,glm-5.4",
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
    "glm-workers.json": {"primary": "minimax,MiniMax-M3.1-Flash"},
    "config.json": {"primary": "zai,glm-5.4"}
  }
}
```

The consumer's primary and any `additional_routes` go first, followed by the
shared chain with duplicates removed. The final shared route stays last.
Those providers must be defined in that consumer. Shared provider definitions
replace providers with the same name. Other provider definitions remain local,
so one listener can use native Responses while another uses Anthropic for Z.ai.
A consumer must not define the same provider name twice, and the shared list
must not either; duplicates are rejected instead of silently keeping the last
entry. Consumer names must be plain JSON basenames: path components, glob
metacharacters and the two policy file names are all rejected at load.

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
`check` never modifies anything. `sync` validates all registered consumers
before writing and replaces each changed file atomically, preserving its file
mode and writing through symlinked consumers. Updates across multiple files are
not a transaction; if an I/O failure interrupts a sync, run `check` and sync
again before restart. An interrupted run can strand a dot-prefixed staging file;
both commands report leftover staging files beside registered consumers —
including beside the symlink-resolved target when a consumer is a link — and
never delete anything, so remove reported leftovers manually. The reported hash
identifies the policy, not the state of a running listener.

Back up existing configurations before the first sync. Keep credential values
in private runtime storage, never in the policy. Shared providers require an
environment-variable reference. The catalog and config only establish route
availability in configuration; verify account access with a live request.

On the main workstation, `scripts/ccr_fallback_policy.py` additionally pins the
route order, registered listener files and consumer primaries in source. The
JSON policy remains derived operator state, not the approval authority. This is
deliberate: an agent may repair credentials or resynchronize consumers, but
cannot expand routing by editing `fallback-policy.json`. A route expansion
requires a reviewed source-policy change, tests and a live credential preflight.
Derived consumers also remove providers and model aliases whose routes are not
active, preventing a dormant provider from becoming an accidental future tier.
Each unambiguous tier model additionally gets a derived bare-id alias
(`<model>` → `<provider>,<model>`): the `/v1/models` listing advertises bare
ids that the router otherwise silently dispatches to `Router.default` (a
requested `MiniMax-M3.1-Flash-Preview` was measured serving `zai,glm-5.3`
before the alias existed). An operator alias entry is never overridden, and a
model name offered by two providers stays unaliased rather than guessing.
The approved main-machine shared chain is MiniMax `MiniMax-M3.1-Flash`,
GLM-5.4, DeepSeek `deepseek-flash`, Meta Model API
(`meta,muse-spark-1.3`, added 2026-10-23 as the last cloud tier on a key
with free credits), the keyed B550 Strata Coder
(`strata-b550,qwen3.8-flash-next-coder`), and resident
`local27,qwen3.8-27b-mxfp4`, with no private consumer seat. GLM-5.4 is
the main and auth primary and the Z.AI vision head: it reads images, while the
superseded GLM-5.3 is text-only. It is the only Z.AI tier in the chain, so a
rate-limited GLM falls through to the next tier instead of a second GLM model.
On 2026-11-23 every consumer primary moved back to `zai,glm-5.4`: the
scoped-worker primary moved off MiniMax and the OCR lane moved off its
OpenRouter code-review primary, by user direction that GLM-5.3 must never be
served again. MiniMax stays first in the shared chain and OpenRouter stays
available to the OCR consumer as a direct-only seat, so an exhausted Z.AI
window degrades a lane instead of ending it. `Provider.models` is an
advertisement, not an access control: an explicit client pin such as
`zai,glm-5.3` was dispatched to that provider with that model id on the wire
and the retired model answered it. The `zai` provider therefore carries a
provider-level `modelMap` that rewrites `glm-5.3`, `glm-5.3-flash`, and
`glm-5.3-flashx` onto `glm-5.4` at dispatch time, so a stale client pin is
served the successor. Evidence: `evidence/glm54-cutover-20261123.md`.
The scoped-worker
primary had moved to MiniMax on 2026-10-21 by user authorization: the Z.AI GLM
plan kept exhausting its monthly utilization window, so GLM Flash moved
behind MiniMax instead of burning that window first on every scoped-worker
dispatch. Since 2026-10-24, the installed worker launcher adds a Qoder lane
before this HTTP chain for fresh offline briefs and offline read-only resumes.
Qoder requests `DeepSeek-Flash` (`dfmodel`, DeepSeek V4.1 Flash) at maximum reasoning.
Read-only briefs expose only Read/Glob/Grep;
`--write-workspace` briefs with declared
`--owned-path` entries run in an isolated git worktree with Read/Glob/Grep/
Write/Edit, no Bash or network, and only the declared paths are applied back.
This lane is owned by `ccr-worker-launcher`; it deliberately has no
`Router.tiers` entry, provider definition, credential placeholder, catalog
slug, or listener restart. A clean Qoder result bypasses CCR, while all other
eligible paths use the governed MiniMax-first chain above. Both GLM tiers were
cordoned 2026-10-18 after the Z.AI monthly tool quota reported 4000/4000
used, then restored the same day by user direction once that premise was
disproved: Z.AI meters usage without distinguishing model sessions from tool
use, the router kept completing GLM calls (by the thousand) after the probe
read zero, as did the ZCode app on the same plan, and the observed completion
limits are the rolling 5-hour prompt window and concurrency caps, so the
cordon's guaranteed-refusal rationale did not apply. The Qwen Token Plan lane
was removed from the chain, and from the main/auth `qwen,qwen3.8-max` seat,
on 2026-10-19: its monthly utilization window was exhausted, so every dispatch
paid one guaranteed refusal before reaching a tier that could still serve. An
exhausted window is not something routing can optimize around; the lane is
skipped until it recovers. Qwen and native GMI stay registered but unrouted,
as do the superseded GLM-5.3, GLM-5.3-Flash, and former FlashX heads, so a
dated re-approval restores them without rewiring credentials. DeepSeek
returned to the chain on 2026-10-21 after its
balance was topped up and a live request passed. The B550 Strata service is
single-flight with a 16384-token context; local27 remains the final tier for
over-long or unavailable B550 requests. MiniMax
canonicalizes M3-family and legacy preview requests to the official
`MiniMax-M3.1-Flash` ID.
Tetra stays registered but unrouted as of 2026-11-23 until live host
authorization, `com.kearm.tetra-vllm-tunnel`, its credential reference, and
endpoint health are restored together. Its prepared endpoint is loopback-only
on Tetra and would forward `127.0.0.1:18317` to Tetra `127.0.0.1:8317`. Its
provider remains single-flight, forces `reasoning_effort: none`, and injects a
missing 32768-token output limit. The harmless bearer credential exists because
shared providers require an environment-backed reference; the remote service
itself remains unauthenticated and must stay reachable only through the SSH
tunnel. Tetra is not a normal local tier because Elliot may reclaim or stop the
guest service at any time; CCR never manages the remote engine.

A mixed task splits along the lane boundary instead of becoming one broad
network-authorized or broad-write worker. The three-line decomposition ledger
still governs it: `Workers:` holds the offline analysis or scoped write,
`Parent-only:` holds only closed-list work such as the acceptance decision,
and `Terminal budget:` covers launching the worker and running acceptance.
The parent or a separately authorized network worker fetches the primary
evidence first; an offline Qoder worker then digests, compares, or
synthesizes it, or generates a scoped patch; a `--write-workspace` brief
names the exact task-owned paths through `--owned-path`; and the parent runs
the acceptance command and inspects the diff only as the final accept/reject
step. Qoder never writes production configuration, model routing,
credentials, git remotes, or live services, and a clean Qoder result is
evidence for parent acceptance, never acceptance itself.

Delegation is not optional for discovery. Path, symbol, and entry-point
discovery is a bounded read-only worker brief, not parent exploration, and
source audit, patch authoring, test running, log and evidence collection, and
drafting findings are worker work. Every actionable user turn starts a fresh
`Workers:`/`Parent-only:`/`Terminal budget:` ledger before progress-making
tools. Qoder ineligibility changes the selected worker lane, never the
decision to delegate: command-requiring verify or repair briefs pass
`--no-qoder-primary` and run on the MiniMax-first chain, while offline source
audits and scoped patch generation use Qoder first. `create_thread` and
native Codex child tasks are never a delegation mechanism; `Workers:` means a
`codex-ccr-worker` run or the named judgment tier.

Before changing or restarting a shared provider route, run:

```bash
uv run --no-project python scripts/ccr_fallback_policy.py preflight --json
```

`preflight` resolves the same private credential material used by service
launch and sends one read-only model-list request directly to each shared
provider using CCR's protocol-specific authorization header. It generates no
completion and never prints credentials or response bodies. A non-2xx/3xx
response, transport failure or unresolved credential is a failed preflight;
the route change or restart must stop there and report the exact provider.
Useful model listing is only an authorization check, not a model-quality or
throughput claim.

## Reuse at startup

Install a single copy of the module and import it from each service launcher.
For example, after making that installed directory importable:

```python
from ccr_fallback_policy import ROOT, serve

serve(ROOT / "glm-workers.json", ["start", "--host", "127.0.0.1", "--port", "3457"])
```

`serve` derives that registered consumer at startup, resolves every variable
its derived configuration references from `runtime-credentials.json` in the
policy directory or the inherited environment, and only then rewrites the
consumer configuration and executes the installed `ccr-rust`, resolved from
`CCR_TEST_BINARY`, `CARGO_HOME` or `PATH`. The binary and every referenced
credential value are verified before the configuration is rewritten, so a
failed launch never leaves a rewritten file behind. The credential file must
be private (mode 0600) when present; explicit inherited environment variables
can supply references absent from the file, and the file itself is optional
when they cover every reference. Credentials are injected only for registered
consumers; launching an arbitrary configuration outside the policy directory
runs with the inherited environment alone. Other client settings and
filesystem/network permissions are unchanged.
Worker launchers can use `load_policy` and `derive` to validate approved routes
instead of maintaining their own allowed-model list. A requested model in a
worker receipt is not proof of the upstream used; inspect actual router logs or
per-tier counters.

The module is validated against `MACHINE_SHARED_PROVIDERS` on every load, so an
installed copy of `ccr_fallback_policy.py` that predates a source change will
refuse to start its listeners. After editing this script, copy it to the
launcher's import directory and clear its stale bytecode before restarting.
A launcher that fails with `Machine shared provider definitions do not match
the approved source policy` has a provider-definition mismatch. Check both the
installed module and policy configuration. Update the module and clear stale
bytecode when the installed module is outdated.

For the MiniMax-specific model ID, transformer, validation, and idle-restart
sequence, see
[MiniMax setup and operations](minimax.md#governed-fallback-runbook).

Sync edits files only. CCR loads configuration at startup, so inspect
`ccr_active_requests` and coordinate an idle restart for each affected listener.
Do not interrupt active work to make an updated file appear live. The result's
`restart_required` lists consumers this run actually rewrote; it is empty under
`check`, where the drift appears in `changed` and no file was touched.

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
