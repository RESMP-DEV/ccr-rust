# SenseNova Token Plan provider registration — 2026-10-23

## Scope

Registered SenseTime's SenseNova Token Plan as a config-first, OpenAI-compatible
provider in CCR-Rust: an operator runbook, a copy-paste example config, a
governed shared-provider definition, and tests that pin the provider contract.
SenseNova is registered in the governed policy as `registered_not_routed`: it
has no route in any consumer chain, so no listener behavior changed and no
credential is required yet. No API key was available or used.

This change does not add a transformer, a protocol variant, or a Rust runtime
branch: the upstream speaks OpenAI Chat Completions, and CCR's provider path is
config-only for that protocol.

## Public contract evidence

- `https://platform.sensenova.ai/docs` (official platform docs): base URL
  `https://token.sensenova.ai/v1`; every request carries a Bearer token in the
  `Authorization` header; keys are created at `platform.sensenova.ai/console/keys`;
  documented models include `sensenova-6.8-flash-lite`,
  `sensenova-u1.5-lite`, and `sensenova-u1.5-fast`; request parameters include
  `max_tokens` (default 65535, range 1..65536), `n` (1..7), `thinking`
  (`enabled`/`disabled`), `reasoning_effort`
  (`low|medium|high|max|none`, default `high`), `response_format.type`, and
  `stream_options.include_usage`; the response carries `message.reasoning`;
  the terminal stream event is a usage-only frame (`choices: []`) before
  `data: [DONE]`.
- `https://raw.githubusercontent.com/OpenSenseNova/SenseNova6.8/main/API.md`
  (official `OpenSenseNova` organization): `POST .../v1/chat/completions` with
  `Authorization: Bearer`, `object: "chat.completion"`,
  `usage.prompt_tokens_details.cached_tokens`, `message.reasoning`, the
  documented sampling set, `image_url` inputs (URL or base64 data URI), and the
  error envelope `{"error": {"message": ..., "type": ..., "code": ...}}`.
- `https://raw.githubusercontent.com/OpenSenseNova/SenseNova-Skills/main/README.md`:
  the international host is `token.sensenova.ai/v1` and the mainland-China host
  is `token.sensenova.cn/v1`; the console, key, base URL, and model name must
  come from the same region.
- `https://www.sensenova.ai/token-plan`: the free public beta allows 1,500
  calls per model per 5 hours, with up to 20 API keys per account.

## Live probes (2026-10-23, no credential used)

- `GET https://token.sensenova.ai/v1/models` without auth: HTTP 401,
  `{"error": {"code": 16, "message": "Authorization Not Found"}}`. The listing
  endpoint exists and is auth-gated; it is not a 404.
- `POST https://token.sensenova.ai/v1/chat/completions` with an invalid Bearer
  token: HTTP 401, `{"error": {"code": 16, "message": "Forbidden"}}`.
- The same two probes against `https://token.sensenova.cn` returned the same
  401 shapes, so both regional hosts reach the same auth plane.
- No `X-RateLimit-*` response headers were present on the 401 responses, so
  `honor_ratelimit_headers` has nothing to act on for this provider.

## Implementation

- `docs/sensenova-token-plan.md`: provider runbook (config, credentials,
  reasoning semantics, region pairing, verification, limitations).
- `examples/sensenova.json`: standalone single-provider config.
- `docs/index.md`, `examples/README.md`: entry points for the new guide and
  example.
- `scripts/ccr_fallback_policy.py`: `sensenova` added to
  `MACHINE_SHARED_PROVIDERS` (base `https://token.sensenova.ai/v1`, credential
  reference `${CCR_SENSENOVA_API_KEY}`, `protocol: openai`, model
  `sensenova-6.8-flash-lite`), plus the updated policy integrity pin.
- `scripts/test_ccr_fallback_policy.py`: pins the SenseNova definition, its
  unrouted status, and the live MiniMax `maxInflightTokens: 250000` value that
  had landed in the deployed policy without reaching the repository source.
- `tests/reasoning_normalization_test.rs`: pins that the SenseNova
  `reasoning` field (message and streaming delta) normalizes to CCR's
  `reasoning_content`.
- `tests/test_routing.rs`: end-to-end mocked dispatch proving a `/v1` base URL
  posts to `/v1/chat/completions` with `Authorization: Bearer <key>` and the
  requested model in the body.
- `CHANGELOG.md`: operator-facing summary.

## Governed policy deployment

- Pre-change recovery snapshot `20261023T133202-0700`
  (`ccr-recovery snapshot --label pre-sensenova-registration`) verified clean:
  44 files, no missing or mismatched entries.
- The live policy JSON was rewritten atomically from the reviewed source
  (`atomic_json`) and the source policy was deployed to
  `~/.local/share/ccr-routing/ccr_fallback_policy.py` (the file
  `~/.claude-code-router/ccr_fallback_policy.py` symlinks to).
- SHA-256 parity: repository script and deployed script are identical
  (`80f22dab6c6d4e075dfea0d40a1ead16694abcb67c3a14e8ae8d0acf89dd9884`);
  the live policy body hashes to
  `7500b006045c24aac70ae7e2528b51cb32ca4c19e1925c5626be6c9f828cb253`,
  which equals the source integrity pin.
- `ccr-fallback-policy check --json`: `changed: []`, `restart_required: []`,
  `stale_staging: []`. Every consumer config kept its previous tier chain, so
  no listener restart was required or performed.
- `ccr-fallback-policy preflight --json`: `sensenova` is reported under
  `registered_not_routed` and is not probed, as designed. The preflight run
  itself is not green for unrelated reasons: `local27` refused the connection
  (its MLX backend is not listening) and `strata-b550` reset the connection.
  Both are pre-existing local-tier conditions, not effects of this change.
- `ccr-recovery snapshot --label post-sensenova-registration` produced
  `20261023T133902-0700` (verified, 44 files) and `ccr-recovery doctor`
  reports `drifted_vs_live: []`, `ok: true`, `runtime_ok: true`, with all four
  listeners (3456 main, 3457 worker, 3458 OCR, 3461 auth) reporting the
  expected tier chains.

## Validation

- `python3 scripts/quality.py` — PASS for all 12 checks (format, python-format,
  python-lint, python-typecheck, rust-sloc-ratchet, shellcheck, clippy,
  rust-tests, no-default-features, debug-binary, python-helpers,
  vendored-gp-routing); receipt `build/reports/quality/receipt.json` recorded
  `aggregate_passed: true` against source revision
  `458c4a33759063739800d3d41f4a68e24e314a46` with the dirty file list.
- `python3 -m unittest discover -s scripts -p 'test_ccr_fallback_policy.py' -v`
  — 39 tests, OK, including `test_sensenova_token_plan_is_registered_but_unrouted`.
- `cargo test --all-features --locked --test test_routing
  sensenova_base_url_dispatches_chat_completions_with_bearer_auth` — 1 passed.
- `cargo test --all-features --locked --test reasoning_normalization_test` —
  5 passed, including `test_sensenova_reasoning_alias_normalizes_to_reasoning_content`.
- `jq empty examples/sensenova.json` — exit 0.
- `CCR_SENSENOVA_API_KEY=dummy ccr-rust --config examples/sensenova.json validate`
  — valid; 1 provider, 1 model, 1 tier (`sensenova,sensenova-6.8-flash-lite`).
- `git diff --check` — clean.

## Worker audits

Two read-only MiniMax workers were run for this task; both reports are
evidence about the inspected surface, not decisions.

- Run `20261023-132506-57ymxv2u` (config-to-router dispatch audit) confirmed
  that no provider-specific normalization is required for an OpenAI-compatible
  upstream, that the `reasoning` alias already exists in the wire types, and
  that a registered-but-unrouted shared provider never reaches a consumer
  config and therefore needs no credential.
- Run `20261023-132511-9lo55n4k` (public contract and test coverage audit)
  fetched the official platform docs, ran the unauthenticated probes
  independently, and flagged that the runbook's claim that SenseNova does not
  document an OpenAI `reasoning_effort` contract was wrong. The runbook was
  corrected: the provider documents `low|medium|high|max|none` with `high` as
  the default, while CCR also permits `minimal` and `xhigh`, which this
  provider does not document.

## Non-claims

- No SenseNova completion was executed. Endpoint reachability, the 401 error
  envelope, and the `/v1/models` route's existence are proven; a real
  completion, streaming behavior, tool calling, vision input, and quota
  consumption are not.
- The model id `sensenova-6.8-flash-lite` is the documented and
  console-shown id, but the served model string in a live response was not
  observed.
- The documented terminal stream event is a usage-only frame
  (`choices: []` plus `usage`) without the `id`/`object`/`created`/`model`
  fields CCR's chunk type requires. The public docs present that frame as an
  excerpt, so the live wire was not proven either way. If the real event omits
  those fields, the frame would not parse as an OpenAI chunk, the provider
  usage would be dropped in favor of an estimate, and the frame would pass
  through to the client. Confirm with one live streaming call before trusting
  SenseNova token accounting in metrics and cost estimates.
- SenseNova has no route in any consumer chain, so it currently serves no
  traffic. It cannot serve traffic until a key exists and a dated route
  approval lands in `MACHINE_ROUTE_CHAIN`.
- No credential was created, stored, or transmitted. The policy holds only the
  `${CCR_SENSENOVA_API_KEY}` reference, and the runtime credential file was
  not modified.
- The repository source now carries the live MiniMax `maxInflightTokens:
  250000` value that previously existed only in the deployed copy. That field
  is not implemented by the `Provider` struct in this source tree, so it is
  carried as operator policy state, not as a tested router feature.

## Remaining scope

Live qualification is blocked on the API key. When `CCR_SENSENOVA_API_KEY`
exists in `~/.claude-code-router/runtime-credentials.json` (mode 0600), the
remaining steps are: `ccr-fallback-policy preflight --json` to authenticate the
credential, a dated decision on the chain position, a source-policy change
with the route, integrity-pin recompute, `ccr-fallback-policy sync --json`, an
idle-gated listener restart via `ccr-wait-idle`, and a live
`scripts/ccr_provider_smoke.py --models sensenova,sensenova-6.8-flash-lite`
run that also checks the streaming usage frame.

## Landing

The implementation landed in RESMP-DEV/ccr-rust commit
`464ce0ad8b7b2eb4fa91b93deaf89ef5b64ca476`
(`feat: add sensenova token plan provider`) and was pushed to the owned
`origin/main` remote.
