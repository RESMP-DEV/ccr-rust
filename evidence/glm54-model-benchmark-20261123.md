# Live per-model benchmark through CCR: 2026-11-23 (UTC)

## Scope

A live, per-model benchmark of four routes ran through a temporary, isolated
CCR listener and produced four durable scorecards. This receipt records what
was run, what the artifacts contain, and what the numbers do not prove. No
live request was re-run while writing it; every figure below is copied from
the scorecards and the listener log that the run itself produced.

The benchmark compares routes on one fixed, checkable prompt suite. It is a
routing and provider-evidence artifact, not a model-quality ranking.

## Run identity

| Field | Value |
| --- | --- |
| Executed | 2026-11-22 21:40 to 21:44 PDT, 2026-11-23 05:40 to 05:44 UTC |
| Repository | `ccr-rust` worktree `glm54-supersede`, branch `feature/glm54-supersede-benchmark` |
| Source revision | `51f6490bdc2c634341ed2583965afa3709f4f423` (`51f6490`), working tree dirty with 21 paths at run time |
| Listener binary | installed `ccr-rust` at `/Users/kearm/.cargo/bin/ccr-rust` (built 2026-11-20 04:49 local), not a binary built from this worktree |
| Harness | `scripts/ccr_model_benchmark.py`, `scripts/ccr_model_benchmark_suite.py`, `scripts/ccr_model_benchmark_transport.py`; all three untracked in git at run time |
| Environment recorded in every scorecard | `macOS-26.7.1-arm64-arm-64bit-Mach-O`, hostname `Mac.lan`, Python `3.15.2` |

The harness files were untracked and the listener ran an installed binary two
days older than the branch head, so the artifact set is not a reproducible
build of this worktree.

## Command shape

Two commands, run in this order, from the repository root.

Listener start, isolated from every shared listener on the machine:

```
ccr-rust --config build/reports/glm54/model-benchmark-runtime/config.json start -p 3462
```

Benchmark run against that listener:

```
python3 scripts/ccr_model_benchmark.py \
  --base-url http://127.0.0.1:3462 \
  --models zai,glm-5.4 \
  --models zai,glm-5.3 \
  --models zai,glm-5.3-flash \
  --models minimax,MiniMax-M3.1-Flash \
  --reps 3 \
  --vision-capable zai,glm-5.4 \
  --vision-capable zai,glm-5.3-flash \
  --out-dir build/reports/model-benchmark
```

The flags recorded inside every scorecard's `config` block match that shape:
`base_url` `http://127.0.0.1:3462`, `repetitions` 3, `streaming` true,
`temperature` 0.0, `timeout_seconds` 120.0, `snippet_execution` true,
`max_output_tokens_override` 0, and `vision_capable_routes` exactly
`zai,glm-5.4` and `zai,glm-5.3-flash`.

## Route set and provider wiring

The temporary config at
`build/reports/glm54/model-benchmark-runtime/config.json` declared exactly
two providers and no others:

| Provider | Upstream | Protocol | Models | Credential |
| --- | --- | --- | --- | --- |
| `zai` | `https://api.z.ai/api/v1` | `responses` | `glm-5.4`, `glm-5.3`, `glm-5.3-flash` | `${CCR_ZAI_API_KEY}` placeholder, never printed |
| `minimax` | `https://api.minimax.io/anthropic/v1` | `anthropic` with the `minimax` transformer | `MiniMax-M3.1-Flash` | `${CCR_MINIMAX_API_KEY}` placeholder, never printed |

Its `Router` block set `default` and `modelAliases.ccr-primary` to
`zai,glm-5.4`, set `tiers` to `["zai,glm-5.4", "minimax,MiniMax-M3.1-Flash"]`
with `strictTierOrder` true, and disabled retry sweeps (`maxSweeps` 0).

## Listener isolation

The run used a dedicated config file, a dedicated port, and a dedicated log,
all under `build/reports/glm54/model-benchmark-runtime/`. It did not touch the
shared listeners on 3456, 3457, 3458, or 3461, and it did not change the
deployed routing policy, the installed configuration, or any live service.

The listener started at `2026-11-23T05:40:51.455486Z` and logged
`CCR-Rust local listener on 127.0.0.1:3462; client API key is not required
here`. It was stopped after the benchmark with SIGTERM, logged at
`2026-11-23T05:44:56.542427Z` as `Received SIGTERM` followed by
`Received shutdown signal, draining connections (timeout 30s)`.

Stop verified after the run: the recorded pid 66319 no longer exists
(`kill -0 66319` reports no such process), nothing listens on 3462
(`lsof -nP -iTCP:3462 -sTCP:LISTEN` is empty), and a TCP connect to
`127.0.0.1:3462` is refused.

## Scenario suite

All four scorecards carry identical `suite_digest` values, which proves the
same prompt bytes were sent to every route. The digest hashes
`scenario.prompt` only, not the checkers, fixtures, or expected values:

| Scenario id | Digest |
| --- | --- |
| `arithmetic_chain` | `ee03ac3019de8b4e` |
| `code_fix` | `a13e8dc739832fcf` |
| `exact_instruction` | `a902cddd130cd8ab` |
| `strict_json` | `b387298701da6fc1` |
| `vision_two_color` | `f97183820f0d0988` |

Each scenario was attempted three times per route, which gives 15, 12, 15,
and 12 attempted runs across the four routes and 54 in total. The two routes
not declared vision-capable skipped their three vision runs, which is why
their attempted counts are 12 rather than 15.

## Headline results

| Route | Scorecard | Accuracy | Passed/attempted | Skipped | Failure rate | Median TTFT | Median wall |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `zai,glm-5.4` | `build/reports/model-benchmark/zai_glm-5.4.json` | 1.000 | 15/15 | 0 | 0.000 | 2.450006 s | 2.450222 s |
| `zai,glm-5.3` | `build/reports/model-benchmark/zai_glm-5.3.json` | 1.000 | 12/12 | 3 | 0.000 | 1.857454 s | 1.857677 s |
| `zai,glm-5.3-flash` | `build/reports/model-benchmark/zai_glm-5.3-flash.json` | 0.667 | 10/15 | 0 | 0.333 | 2.125501 s | 1.457606 s |
| `minimax,MiniMax-M3.1-Flash` | `build/reports/model-benchmark/minimax_MiniMax-M3.1-Flash.json` | 0.833 | 10/12 | 3 | 0.167 | 1.263485 s | 1.117506 s |
| **All routes** | four scorecards plus `build/reports/glm54/model-benchmark-runtime/benchmark.json` | 47/54 = 0.870 | 47/54 | 6 | 7/54 = 0.130 | not aggregated | not aggregated |

Two medians in that table are not comparable without the denominator note
below. The harness computes `median_ttft_seconds` over attempted runs that
reported a TTFT and `median_wall_seconds` over all attempted runs
(`scripts/ccr_model_benchmark.py:225`, `aggregate_runs`). On the two routes
with failures, those two medians therefore describe different run sets, and
for `zai,glm-5.3-flash` the TTFT median is higher than the wall median
because the fastest runs are the ones whose TTFT was never recorded.

The task brief described `zai,glm-5.3-flash` as 8/12. The scorecard does not
support that figure and this receipt follows the artifact: 10 of 15 attempted
runs passed, which is 9 of 12 text-only attempts plus 1 of 3 vision attempts.
There is no subset of the flash scorecard that totals 8/12.

## Per-scenario outcomes

Median TTFT and median wall in seconds, per scenario, as recorded in each
scorecard's `summary.per_scenario` block.

| Scenario | `zai,glm-5.4` | `zai,glm-5.3` | `zai,glm-5.3-flash` | `minimax,MiniMax-M3.1-Flash` |
| --- | --- | --- | --- | --- |
| `exact_instruction` | 3/3, ttft 2.662679, wall 2.722063 | 3/3, ttft 2.213236, wall 2.215709 | 0/3, ttft null, wall 1.303905 | 1/3, ttft 0.877327, wall 0.877644 |
| `strict_json` | 3/3, ttft 1.529941, wall 1.530572 | 3/3, ttft 1.809278, wall 1.809471 | 3/3, ttft 1.978672, wall 1.978965 | 3/3, ttft 0.993750, wall 1.028302 |
| `code_fix` | 3/3, ttft 1.513967, wall 1.514229 | 3/3, ttft 1.905630, wall 1.905884 | 3/3, ttft 3.666087, wall 3.666442 | 3/3, ttft 1.415444, wall 1.478946 |
| `arithmetic_chain` | 3/3, ttft 0.996766, wall 0.997049 | 3/3, ttft 1.594568, wall 1.594922 | 3/3, ttft 1.449944, wall 1.457606 | 3/3, ttft 1.400513, wall 1.410283 |
| `vision_two_color` | 3/3, ttft 4.616418, wall 4.616777 | skipped x3, text-only route | 1/3, ttft 1.267503, wall 1.277209 | skipped x3, text-only route |

The two skipped blocks carry an explicit reason in every run record:
`skipped: route zai,glm-5.3 is treated as text-only; declare it with
--vision-capable to send the image fixture`, and the equivalent sentence for
`minimax,MiniMax-M3.1-Flash`. The skipped runs carry no pass or fail signal
and are excluded from the accuracy denominator.

All 7 failed runs carry `error_class` `check_failed` and no other error
class appears in any scorecard. The failure details are:

| Route | Scenario | Repetitions failed | Recorded detail |
| --- | --- | --- | --- |
| `zai,glm-5.3-flash` | `exact_instruction` | 1, 2, 3 | `expected 'bench-ok-42', got ''` |
| `zai,glm-5.3-flash` | `vision_two_color` | 1, 2 | `response mentioned no color word` |
| `minimax,MiniMax-M3.1-Flash` | `exact_instruction` | 1, 3 | `expected 'bench-ok-42', got ''` |

Every failed run recorded upstream usage (`usage_source` `upstream`, 32 output
tokens on the empty-text failures) and a null `ttft_seconds`, so the router
received a successful upstream response that surfaced no text to the client.
That is a response-content observation, not a transport error and not
directly a statement about model capability.

## Which model actually answered each request

The listener log was parsed request by request and paired one to one with the
54 `[PASS]`/`[FAIL]` lines in the benchmark stderr log. All 54 requests
matched in order, with zero route mismatches, so the attribution below is
exact rather than inferred.

| Requested route | Served by the same route | Served by a different route |
| --- | --- | --- |
| `zai,glm-5.4` | 12 | 3 (served by `minimax,MiniMax-M3.1-Flash`) |
| `zai,glm-5.3` | 12 | 0 |
| `zai,glm-5.3-flash` | 15 | 0 |
| `minimax,MiniMax-M3.1-Flash` | 12 | 0 |

The three cross-provider serves are the first three `zai,glm-5.4` runs:
`exact_instruction` repetition 1, `strict_json` repetition 1, and `code_fix`
repetition 1. The log shows why. Two of them follow an explicit Z.AI rate
limit on tier `zai,glm-5.4`:

```
2026-11-23T05:41:17.812112Z  WARN ccr_rust::ratelimit: Rate limited, backing off tier=zai,glm-5.4 backoff_secs=1 consecutive=1 starved=true retry_after=None
2026-11-23T05:41:17.813325Z  WARN ccr_rust::router: Rate limited on zai attempt 1 (retry-after: None)
```

with a second event at `05:41:19.100861Z` and `backoff_secs=2`. The third
cross-provider request was issued while the `zai,glm-5.4` tier was still
starved, so it went straight to the MiniMax tier without a fresh attempt on
Z.AI. Those are the only two rate-limit events in the whole run.

Consequences for reading the scorecards: the `zai,glm-5.4` headline of 15/15
includes three responses produced by MiniMax-M3.1-Flash, so 12 of those 15
runs are attributable to GLM-5.4 itself. The 54 in-order request/response log
lines are the evidence for that split; response formatting is not, because
the Z.AI-served `strict_json` repetition 2 is also compact while repetition 3
is spaced, so formatting does not separate the two servers.

`zai,glm-5.3` and `zai,glm-5.3-flash` were not in the tier list; the router
served them as direct routes (`Direct routing: zai,glm-5.3 (not in tier
list)`), and the log shows each was answered by the `zai` provider with the
requested model, with no fallback.

## Benchmark exit code

The benchmark process exited 1. That is the harness's designed signal for
checker failures, not a harness or transport error: `main()` in
`scripts/ccr_model_benchmark.py:626` counts failed or errored runs and
returns 1 when that count is non-zero, and the run's own stderr ends with
`7 attempted run(s) failed a checker or errored`. The exit code matched the
7 failed runs exactly (5 on `zai,glm-5.3-flash`, 2 on
`minimax,MiniMax-M3.1-Flash`). Every request reached an upstream provider
and every scorecard was written and parsed; there is no missing artifact and
no unhandled error class in the run.

## Evidence index

| Artifact | SHA-256 |
| --- | --- |
| `build/reports/model-benchmark/zai_glm-5.4.json` | `ada08fcc7c927f6925dbf8ccd57f9e76e87977a0e3befbc29efffae6af079b17` |
| `build/reports/model-benchmark/zai_glm-5.3.json` | `b065c2bd45a1236fe5e4e07f60e503292307f63d3ed555144e59515f8a3e1b02` |
| `build/reports/model-benchmark/zai_glm-5.3-flash.json` | `3b4776317d8b0aeadc62e7a94fb01fab7c6d5509474f336efdb2509fc6824695` |
| `build/reports/model-benchmark/minimax_MiniMax-M3.1-Flash.json` | `743c636c9aff9224c9b358a5e74bf46d2d8bea5361f18220d32fa39460a2fe52` |
| `build/reports/glm54/model-benchmark-runtime/benchmark.json` | `02ee53db9217adc8482e5fa05ea96cc0c0e7ea11004bec63b16d8e7d3f427f20` |
| `build/reports/glm54/model-benchmark-runtime/listener.log` | `8a84474d6c1b24b322ad610229684ca171fc23535726dece145867d79f2340c0` |
| `build/reports/glm54/model-benchmark-runtime/benchmark.stderr.log` | `05285a460eaf3ee5c0e236d7c199c3939d0944c42436533f296556f72b8de14a` |
| `build/reports/glm54/model-benchmark-runtime/config.json` | `1a76d2db7923e2ae89c56ce2347ab462b1dd50e7f0871f88446f03720d89b113` |

`build/reports/model-benchmark/` also holds two earlier dry-run scorecards,
`zai_glm-5.4.dry-run.json` and `zai_glm-5.3-flash.dry-run.json`, both with
`mode` `dry-run`. They are pre-flight artifacts written without contacting
the router and are not part of this live run.

All files under `build/` are git-ignored (`.gitignore:29`, `/build/`), so the
scorecards and logs are machine-local and will not survive a clean of the
worktree. This receipt is the durable copy of the findings; it carries the
digests above so a future reader can tell whether the machine-local artifacts
still match.

## Non-claims

- These scores come from one fixed prompt suite at one moment in time. They
  do not measure general model quality, reasoning depth, or long-horizon
  agentic behavior, and no ranking here is a statement about which model is
  better for a real workload.
- Latency figures describe this machine, this listener, and this network
  path. They are not provider-intrinsic numbers, and the GLM-5.4 vision
  repetition 3 run at 26.9 s shows how much a single request can move a
  median.
- Three of the fifteen `zai,glm-5.4` runs were served by MiniMax-M3.1-Flash
  after Z.AI rate limits, as shown above. The 15/15 headline for GLM-5.4 is
  therefore a property of the tier configuration under a live 429, not a
  clean 15/15 GLM-5.4 measurement.
- Skipped scenarios contribute no accuracy signal. `zai,glm-5.3` and
  `minimax,MiniMax-M3.1-Flash` have no vision result at all, so their
  12/12 and 10/12 cover four text scenarios only.
- The two routes that failed `exact_instruction` did so with an empty client
  response while upstream usage was still reported. The run does not isolate
  whether that comes from the model, the provider, or the CCR response
  translation; it only records that the client saw empty text.
- The listener served an installed binary built on 2026-11-20, not a build of
  this worktree's head, and the harness scripts were untracked in git at run
  time. The artifact set does not pin a reproducible source revision.
- Token counts come from upstream-reported usage where the route reports it.
  The listener logged 21 `CRITICAL token drift` warnings, 15 on MiniMax and 6
  on Z.AI; the Z.AI entries carry the vision request's 167-token local
  estimate against 56 upstream tokens. Tokens per second uses output tokens
  and wall time minus TTFT, and the successful records report upstream usage,
  so the drift does not by itself make the reported throughput approximate.
- The `code_fix` checker executes model-generated Python in a local
  subprocess with a timeout. That is not a security sandbox.
- The vision scenario is skipped unless the route is declared
  vision-capable, so an undeclared route was never sent an image fixture.
- The suite digests fix the prompt bytes, not the provider responses. The
  same digest over four routes does not make the four routes comparable in
  every other respect.
- Nothing here proves which route to select for production traffic. That
  decision needs a rerun under the deployment's own provider configuration,
  and this run's own GLM-5.4 numbers show how a provider-side rate limit can
  change a scorecard.

## What this supports

The run supports a narrow set of statements: `zai,glm-5.4` answered every
scenario it was actually served on the live CCR path, including 3 of 3
two-color image checks; `zai,glm-5.3` answered all 12 text runs; and both
`zai,glm-5.3-flash` and `minimax,MiniMax-M3.1-Flash` produced empty client
text on the exact-instruction scenario while the router reported upstream
success.

It does not by itself support a promotion of any route in the governed
fallback policy, because the GLM-5.4 tier configuration in this run was
different from the deployed one, the listener was a temporary instance, and
the measurement window was under four minutes.
