# MiniMax setup and operations

Use this page when adding MiniMax to an ordinary CCR configuration, running
several MiniMax credentials, or operating the main workstation's shared
fallback chain. The two hard requirements are easy to miss:

1. Use `MiniMax-M3.1-Flash-Preview`, not the shorter M3.1 alias.
2. Configure `transformer: {use: ["minimax"]}` explicitly. CCR does not select
   a transformer from the provider name.

Skipping either requirement can still return HTTP 200 while serving the wrong
model or omitting all MiniMax compatibility handling.

## Choose the setup path

| Situation | Correct path |
| --- | --- |
| One ordinary CCR config | Add the provider fragment below, add its exact route, validate, start or restart that router, and run a representative tool round trip. |
| Multiple MiniMax credentials or a custom provider name | Use separate provider names and credential variables. Keep the MiniMax API host and explicit transformer. Names containing `minimax`, or a MiniMax API host, receive M3-family canonicalization. |
| Main workstation shared fallback | Change `scripts/ccr_fallback_policy.py` in a reviewed source change. Deploy the source module to the service root, align the derived policy, run policy tests and preflight, sync consumers, and restart only idle listeners. See [Shared fallback policy](shared-fallback-policy.md). |
| Existing route behaves oddly | Check the exact model ID, transformer entry, running process image, live route counters, and a real tool call. A model catalog or health response alone is not acceptance. |

## Provider fragment

For a standalone configuration, start with this provider fragment:

```json
{
  "name": "minimax",
  "api_base_url": "https://api.minimax.io/anthropic/v1",
  "api_key": "${MINIMAX_API_KEY}",
  "protocol": "anthropic",
  "auth_header": "authorization",
  "models": ["MiniMax-M3.1-Flash-Preview"],
  "transformer": {"use": ["minimax"]}
}
```

Add a matching route to `Router.default` or `Router.tiers`:

```json
"minimax,MiniMax-M3.1-Flash-Preview"
```

The router process must have `MINIMAX_API_KEY` in its environment. CCR does
not load a `.env` file itself. Keep credential values out of the config and
out of shell history.

For a second credential, use a distinct name and variable, for example
`minimax-personal` with `${MINIMAX_PERSONAL_API_KEY}`. Keep the same API host
and transformer entry. Separate CCR configs, ports, logs, and persistence
namespaces are preferable when the plans must not share quota or fallback.

## Model IDs and provider names

`MiniMax-M3.1-Flash-Preview` is the recommended and contractual M3.1 ID. A
direct upstream request for `MiniMax-M3.1-Flash` returned HTTP 200 while
serving `MiniMax-M3`; the explicit preview ID returned HTTP 200 and served the
requested model. Therefore a status code does not distinguish these IDs.

CCR recognizes the M3 family as:

- exact `MiniMax-M3` or `minimax-m3`, case-insensitively;
- any model beginning with `MiniMax-M3.`;
- the exact preview ID above.

For a MiniMax provider, CCR sends M3-family requests as
`MiniMax-M3.1-Flash-Preview`. It injects only Anthropic-style
`thinking: {type: "adaptive"}` and does not add the OpenAI-only
`reasoning_split: true`.

Provider detection is not limited to a provider literally named `minimax`. A
provider name containing `minimax`, such as the shipped
`minimax-anthropic` example, or a MiniMax API host receives the same M3-family
handling. Dispatch canonicalization and pricing resolution share one predicate,
so a differently named MiniMax provider does not lose canonical pricing.
Model-keyed transformer overrides still resolve by the requested route name;
do not rely on that as a reason to keep an ambiguous model alias.

M2.x models are a different compatibility family. They keep the
`reasoning_split` handling and are not canonicalized to the M3.1 preview ID.
Check MiniMax's current model reference before assigning capabilities or
context limits across generations.

## Required transformer

The MiniMax transformer is required for:

- adaptive thinking injection;
- canonicalization on paths that invoke the transformer directly;
- malformed assistant-history cleanup;
- complete-response cleanup;
- streaming transport-corruption cleanup.

The registry does not infer a transformer from `provider.name`. A provider
without the explicit transformer entry builds an empty chain. This was a real
production defect: the MiniMax transformer existed in the registry but none of
its behavior ran until the shared policy pinned its `use` entry.

## Malformed-output behavior

Observed MiniMax transport corruption includes quote-only assistant text, a
provider delimiter, an embedded NUL, and tool tags carrying MiniMax's zero-width
transport marker. The transformer removes such text from assistant history
while preserving `tool_use` blocks and their adjacent tool-result pairing. User
messages are never rewritten.

If malformed text is removed and neither visible text nor a usable `tool_use`
block remains, CCR inserts `[MALFORMED_MINIMAX_OUTPUT_REMOVED]` to keep the
turn structurally valid. Complete responses use the same text rules. Streaming
deltas are fragments, so ordinary whitespace and quotation marks are preserved;
only a confirmed transport marker is cleared.

This is transport hygiene, not a claim that MiniMax output quality is uniformly
good. A clean mocked test or short live window does not prove all future
responses are clean.

## Standalone validation

After configuring the provider and route:

```bash
ccr-rust --config /path/to/config.json validate
```

Start or restart the intended router, then check its loopback health endpoint.
Use the actual port from that config:

```bash
curl --fail --silent http://127.0.0.1:3456/health
curl --fail --silent http://127.0.0.1:3456/v1/models
```

`/health` proves the listener is alive. `/v1/models` proves the configured
catalog is exposed, not that the account can complete a request. Finish with a
representative client tool round trip as described in
[Local operation](local-operation.md#prove-the-selected-path). For MiniMax,
also confirm that a non-streaming response echoes the exact preview model ID.

## Governed fallback runbook

This section applies to the main workstation policy consumed by
`~/.claude-code-router/serve*.py`. The source authority is
`scripts/ccr_fallback_policy.py`; the JSON policy and generated consumer
configs are derived state.

1. Make and review the source change. Route or shared-provider expansion is
   not an edit to `fallback-policy.json`.
2. Run the policy tests:

   ```bash
   uv run --no-project python -m unittest discover -s scripts \
     -p 'test_ccr_fallback_*.py'
   ```

3. Build and install the reviewed router binary if the change includes Rust.
4. Deploy the reviewed policy module to the service root. Launchers import
   their installed copy, not the Git worktree. Back up the previous installed
   copy first:

   ```bash
   cp scripts/ccr_fallback_policy.py \
     ~/.claude-code-router/ccr_fallback_policy.py
   ```

   Clear the service root's stale `__pycache__` entries for that module. A
   stale installed copy makes startup fail with a shared-provider mismatch.
5. If `MACHINE_SHARED_PROVIDERS` changed, align the corresponding provider
   definitions in `~/.claude-code-router/fallback-policy.json` with the
   reviewed source. `sync` validates this equality before rewriting consumers;
   it is not a route-expansion bypass.
6. Run preflight and resolve every failed provider before changing a route:

   ```bash
   uv run --no-project python scripts/ccr_fallback_policy.py \
     --root ~/.claude-code-router preflight --json
   ```

7. Run `check`, then `sync`:

   ```bash
   uv run --no-project python scripts/ccr_fallback_policy.py \
     --root ~/.claude-code-router check --json
   uv run --no-project python scripts/ccr_fallback_policy.py \
     --root ~/.claude-code-router sync --json
   ```

8. Restart each affected listener only when its `ccr_active_requests` gauge is
   zero. Do not interrupt active reviewer, worker, or client streams.
9. Re-run health, catalog, exact-model, and representative tool-call checks on
   every restarted listener.

A failing provider in preflight blocks a route change. It does not by itself
block a binary-only swap that leaves the route set unchanged, but the failure
must still be recorded and resolved separately. For example, DeepSeek HTTP 402
is an account-funding condition, not a MiniMax routing result.

## Failure triage

| Symptom | First checks |
| --- | --- |
| HTTP 200 but the served model is `MiniMax-M3` | Request the exact preview ID; inspect a non-streaming model echo rather than trusting status. |
| No adaptive thinking or malformed-output cleanup | Confirm `transformer: {use: ["minimax"]}` in the active config and that the listener restarted after the change. |
| Worker startup fails with a shared-provider mismatch | Compare both the installed `~/.claude-code-router/ccr_fallback_policy.py` and derived provider definitions with reviewed source. Deploy the module and clear stale bytecode only if it is outdated. |
| Config edit has no effect | CCR loads config at startup. Restart the listener when it is idle. |
| Tools are unreliable after malformed visible text | Confirm the current binary and transformer are active, then inspect router logs and a fresh client rollout. A self-reported model is not route proof. |
