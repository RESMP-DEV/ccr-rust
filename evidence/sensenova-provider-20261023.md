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

## Activation — 2026-10-23 (second session, after the key was issued)

The SenseNova key was issued later the same day and stored in the governed
credential store, never in a config file, a document, a log, or a commit:
`~/.claude-code-router/runtime-credentials.json` (mode 0600) now holds
`CCR_SENSENOVA_API_KEY`, written atomically with a staging file, `fsync`, and
`os.replace` after refusing to run against a non-0600 target.

### Live qualification before any route change

- `GET https://token.sensenova.ai/v1/models` with the stored credential:
  HTTP 200. The account catalog is `sensenova-6.7-flash-lite`,
  `sensenova-u1-fast`, `sensenova-6.8-flash-lite`, and `sensenova-u1.5-lite`.
- A scratch CCR listener on port 3472 (separate from every production port)
  served the provider from `examples/sensenova.json`:
  `python3 scripts/ccr_provider_smoke.py --base-url http://127.0.0.1:3472
  --models sensenova,sensenova-6.8-flash-lite --prompt 'Reply with
  SENSENOVA_OK.' --max-output-tokens 512 --verbose` reported 4/4 PASS for
  chat/non-stream, chat/stream, responses/non-stream, and responses/stream.
  `GET /v1/latencies` on that listener showed one tier, `ccr-sensenova`, with
  4 samples and a 1.97 s EWMA.
- A direct upstream streaming call showed the real wire shape of the terminal
  usage frame: it carries `id`, `object`, `created`, `model`, `request_id`, and
  `usage` alongside an empty `choices` array, so it parses as an OpenAI chunk
  and CCR keeps the provider's usage. The abbreviated excerpt in the public
  documentation is not what the server sends, so the usage-drop risk recorded
  above does not materialize. The same call showed `delta.reasoning` frames,
  which is the field CCR maps to `reasoning_content`.
- A direct upstream tool request returned `finish_reason: tool_calls` with a
  well-formed `get_weather` call and `{"city": "Paris"}` arguments, so the
  provider supports function calling.

### Route decision and deployment

- The route was added to the governed chain directly behind MiniMax and ahead
  of Z.AI GLM-5.3 Flash. SenseNova is a free Token Plan lane, so it absorbs
  volume that would otherwise spend the scarce Z.AI window, while MiniMax
  remains the worker primary and GLM-5.3 remains the main and auth primary.
- Source policy `scripts/ccr_fallback_policy.py`:
  `MACHINE_ROUTE_CHAIN` is now
  `minimax,MiniMax-M3.1-Flash` -> `sensenova,sensenova-6.8-flash-lite` ->
  `zai,glm-5.3-flash` -> `deepseek,deepseek-flash` ->
  `strata-b550,qwen3.8-flash-next-coder` -> `local27,qwen3.8-27b-mxfp4`; the
  provider entry carries the live catalog and `tier_name: ccr-sensenova`; the
  integrity pin is
  `6e81d0df79332595d6c84819d5877964b379f665f7200883e8c0c477eebad81d`.
- `ccr-recovery snapshot --label pre-sensenova-route-activation` produced
  `20261023T134612-0700` (44 files, verified, no missing or mismatched
  entries) before any live write.
- The live policy JSON was rewritten atomically from the reviewed source and
  the source policy was deployed to
  `~/.local/share/ccr-routing/ccr_fallback_policy.py`.
- `ccr-fallback-policy preflight --json` authenticated SenseNova:
  `{"provider": "sensenova", "ok": true, "status": 200, "detail":
  "authenticated on models with Authorization"}`. The same run still reports
  `local27` as a connection refusal and `strata-b550` as a connection reset;
  both are the pre-existing local-tier conditions recorded in the registration
  section and are not caused by this change.
- `ccr-fallback-policy check --json` reported
  `changed: ["config.json", "config-auth.json", "glm-workers.json",
  "ocr-reviewers.json"]`, and `ccr-fallback-policy sync --json` rewrote those
  four consumer configs with the SenseNova tier in position two behind each
  consumer's own primary.
- Listener activation is idle-gated through `ccr-recovery activate`, never an
  ungated `launchctl kickstart`.

### Live proof on the governed worker listener

- After the idle-gated worker restart, `POST http://127.0.0.1:3457/v1/chat/completions`
  with the explicit route `sensenova,sensenova-6.8-flash-lite` returned HTTP
  200, served model `sensenova-6.8-flash-lite`, `x-ccr-tier: ccr-sensenova`,
  and the content `SENSENOVA_LIVE_OK`. The first attempt with
  `max_tokens: 128` ended as `finish_reason: length` with the whole budget
  spent on reasoning tokens; `max_tokens: 512` produced a clean `stop`.
- The same listener returned a native tool call for the same provider:
  `finish_reason: tool_calls`, function `get_weather`, arguments
  `{"city": "Paris"}`, which proves CCR's OpenAI tool-call path carries the
  provider's tool calls unchanged.
- The authenticated main listener on port 3461 was restarted idle-gated and
  returned the same proof for the explicit route: HTTP 200,
  `x-ccr-tier: ccr-sensenova`, served model
  `sensenova-6.8-flash-lite`, and content `SENSENOVA_AUTH_OK`. A 512-token
  request ended at `length` after reasoning consumed the cap; a 1024-token
  request ended at `stop`, confirming the output cap must remain generous for
  this thinking-by-default preview model.

### Activation state at this receipt

- Worker (`3457`) and auth (`3461`) restarted idle-gated and serve the new
  chain; their live tier lists and explicit-route probes pass.
- Main (`3456`) and OCR (`3458`) still had active non-streaming requests, so
  the first main `ccr-recovery activate` timed out in the idle stage after 900
  seconds (exit 75) and never restarted the listener. The same safe gate is now
  retrying main and OCR in detached background processes with a 7200-second
  idle timeout; receipts are
  `build/reports/sensenova-activation/main-retry.json` and
  `.../ocr-retry.json`. At the last check, `ccr-recovery doctor` reported
  worker and auth clean and the expected new chain, while main and OCR reported
  `listener has a stale in-memory tier order` because they have not yet
  restarted. The on-disk configs for all four are already correct.

## Workload fit and final routing — 2026-10-23 follow-up

### Dual quota

The official docs define two independent rolling pools: 60,000 points per five
hours and 600,000 per seven days for General Points and for Flash-Lite Points.
Flash-Lite usage drains the dedicated pool first, then spills into General;
image models and any non-Flash-Lite model drain General only. Every Flash-Lite
dedicated point earns a one-to-one General rebate, credited later and valid for
30 days. The two pools are therefore sequential, not additive; Flash-Lite is
the only model family that both has its own pool and manufactures rebate credit
for the general-only image models.

The live account catalog exposed only `sensenova-6.8-flash-lite` for chat plus
`sensenova-u1.5-lite` for actual image generation. `sensenova-6.7-flash-lite`
and `sensenova-u1-fast` were listed by `/models` but returned 404 when called;
docs-only `sensenova-u1.5-fast` returned 403 "not available in the current
token plan". Those stale IDs were removed from the governed provider model
list.

### Measured model behavior

- With no provider override, `sensenova-6.8-flash-lite` reasons by default. A
  simple 140-word writing task burned the full 4,096-token cap on hidden
  reasoning and emitted no answer. The documented string
  `thinking: "disabled"` was rejected, and `reasoning_effort: "none"` alone did
  not stop reasoning.
- The effective non-thinking pair is `thinking: {"type":"adaptive"}` plus
  `reasoning_effort: "none"`. Under that pair the same 140-word task produced
  164-200 output tokens and a clean `stop` in 4.1-4.9 s across three runs,
  with zero reasoning tokens.
- Image extraction through CCR from a 512x512 generated infographic was
  correct (`SENSENOVA QUOTA`, the three bar labels, and the caption) in
  1.6-1.9 s and 72 completion tokens with the non-thinking pair. The same
  request without the override took 6.7 s and 446 completion tokens, of which
  397 were reasoning. That is a 4.1x latency and 6.2x completion-token win for
  extraction on the same model.
- The same image request sent to the existing OpenRouter/Nemotron code-review
  primary returned HTTP 404 "No endpoints found that support image input" and
  fell through to SenseNova; there is no existing image route to reuse.
- `sensenova-u1.5-lite` generated a legible 512x512 infographic in 13.1 s. The
  response usage was 535 input + 4,096 output tokens and one image. At roughly
  4,631 usage units per 512 image, the 60,000 General pool is about 12 such
  images per five-hour window before rebate credit; this is an estimate from
  API usage, not a console point reconciliation.

### Final use case and policy

SenseNova is not a generic coding/review fallback. The code-review listener is
not image OCR; it is the open-code-review client and remains on OpenRouter.
Codex worker traffic also carries built-in tool types SenseNova rejects with a
deterministic 400, so the worker fallback chain was restored to MiniMax -> GLM
Flash -> DeepSeek -> Strata -> local27.

The final policy exposes one direct-only capability on the worker listener:
`sensenova-vision,sensenova-6.8-flash-lite`. It stays in `Providers` but not in
`Router.tiers`, so only an explicit tool-less extraction/vision caller can use
it, while normal worker fallback cannot. The provider forces the measured
adaptive/non-thinking pair. `sensenova` remains registered but unrouted as a
separate capability; `sensenova-vision` is included in preflight and
authenticated HTTP 200.

### Validation added in this follow-up

- `python3 scripts/quality.py` — 12/12 checks passed after the provider
  override and direct-route policy changes.
- `python3 -m unittest discover -s scripts -p 'test_ccr_fallback_policy.py'
  -v` — 41 tests passed.
- `cargo test --all-features --locked --test test_routing
  sensenova_base_url_dispatches_chat_completions_with_bearer_auth` — passed;
  the mock asserts the forwarded body carries both the adaptive thinking object
  and `reasoning_effort: none`.
- `ccr-fallback-policy preflight --json` — `sensenova-vision` authenticated to
  `/models` with HTTP 200.
- `ccr-fallback-policy check --json` — no drift after sync; the worker's
  `Router.tiers` excludes SenseNova while its `Providers` includes the
  direct-only vision entry.

### Non-claims

- The point-per-token rate is not published and not reconciled against the
  console. The image cost above is inferred from API usage fields.
- The rolling five-hour and weekly ceilings were not exhausted. Quota error,
  Retry-After, and sustained concurrency behavior remain unmeasured.
- No production caller has yet been migrated to the direct-only route. The
  session-audit distillation workload is the best existing non-review
  candidate, but changing that external producer remains a separate change.
- No image-editing cadence or 2K/4K quality comparison was run; only one
  512x512 generation per working image model was observed.

### Final live wiring

- The worker config was derived with SenseNova absent from `Router.tiers` and
  `sensenova-vision` present only in `Providers`. An explicit
  `sensenova-vision,sensenova-6.8-flash-lite` image request returned HTTP 200,
  `x-ccr-tier: ccr-sensenova-vision`, the correct infographic labels, 67
  completion tokens, and zero reasoning tokens.
- The daily `session-audit distill --days 2 --verbose` run completed through
  that route and produced four evidence-backed candidates. Its router log
  records `Direct routing: sensenova-vision,... (not in tier list)` and
  `Success on ccr-sensenova-vision after 14.19s`. The code pin is committed in
  `RESMP-DEV/session-audit` as `d67c0b1`.
- The previous installed binary came from the unmerged
  `feature/token-aware-admission` worktree. It was merged into CCR-Rust rather
  than silently discarded, preserving the live MiniMax
  `maxInflightTokens: 250000` behavior. The new binary includes both that
  feature and the SenseNova request override; the worker listener was restarted
  idle-gated and now maps the final installed inode.
