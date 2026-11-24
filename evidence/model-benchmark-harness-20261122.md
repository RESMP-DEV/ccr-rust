# Per-model benchmark harness (2026-11-22)

## Scope

Add an objective per-model benchmark harness that runs a fixed, verifiable
task suite through the local CCR HTTP API and writes durable per-route
scorecards. Python standard library only. No Rust, routing, configuration,
service, or installed-binary change.

Owner surface:

- `scripts/ccr_model_benchmark.py` (runner: CLI, run loop, aggregation, scorecards)
- `scripts/ccr_model_benchmark_suite.py` (fixed task suite, checkers, image fixture)
- `scripts/ccr_model_benchmark_transport.py` (SSE parsing and HTTP transport)
- `scripts/test_ccr_model_benchmark.py` (offline unit tests)
- `docs/observability.md`, `docs/quality-matrix.md`, `docs/index.md`,
  `benchmarks/README.md`, `CHANGELOG.md`

## What was built

Five fixed scenarios, each with one objective expected outcome:

| Scenario | Checker | Objective |
| --- | --- | --- |
| `exact_instruction` | `exact_text` | the whole response equals the requested token |
| `strict_json` | `strict_json` | the whole response is one JSON object with exact keys and value types |
| `code_fix` | `code_fix` | the returned snippet, executed locally in a subprocess, satisfies fixed assertions |
| `arithmetic_chain` | `arithmetic` | the last integer of a multi-step word problem is the expected one |
| `vision_two_color` | `vision` | the first color named for the left block of a generated two-color PNG is correct |

Per run the harness records pass or fail, TTFT, wall time, output tokens,
input tokens, error class, and output text length. Per route it aggregates
accuracy, median TTFT, median wall time, tokens per second, stability, and
failure rate, and it keeps correctness separate from speed. The vision
scenario is skipped with an explicit reason for routes that are not declared
with `--vision-capable`; the code-fix scenario is skipped with an explicit
reason under `--no-exec` instead of executing returned snippets.

## Evidence

Commands run from the repository root on 2026-11-22. The four harness files
were later renamed on the same day to the repository's `ccr_` script
convention; the commands below were re-run under the new names with the same
results, and no harness behavior changed in the rename.

| Command | Exit | Result |
| --- | --- | --- |
| `python3 -m unittest discover -s scripts -p "test_ccr_model_benchmark.py"` | 0 | 66 tests, OK |
| `python3 scripts/ccr_model_benchmark.py --models zai,glm-5.4 --models zai,glm-5.3-flash --reps 3 --dry-run --out-dir build/reports/model-benchmark` | 0 | 5 scenarios parsed per route, 1 explicit skip per route, 2 dry-run scorecards written |
| `ruff check benchmarks scripts tests/provider_validation` | 0 | All checks passed |
| `ruff format --check benchmarks scripts tests/provider_validation` | 0 | 25 files already formatted |
| `ty check scripts` | 0 | All checks passed |
| `python3 scripts/check_sloc.py --json` | 1 | pre-existing: 41,180 Rust nonblank lines against the pinned 41,024 ceiling (this change adds no Rust) |

The offline gate exercises every checker, both PNG fixture properties, the SSE
consumer, the aggregation math, the route parsing rules, the skip rules, and
the dry-run CLI, with no network access.

One end-to-end check was run against a throwaway loopback SSE mock on
`127.0.0.1:39456` (not the live CCR listener, and not part of the committed
test suite). Ten runs completed over two repetitions, per-run verdicts
differed per scenario, TTFT and wall time were measured, upstream usage was
read from the final chunk, and the scorecard was written with accuracy 0.40,
stability 1.00, and failure rate 0.60. Exit code was 1, which is the
documented "at least one attempted run failed a checker" outcome. The mock was
stopped afterwards.

## Baseline state observed, not caused by this change

`python3 -m unittest discover -s scripts -p "test_*.py"` reports 2 failures
and 62 errors without the new test file (182 tests) and 2 failures with 62 or
63 errors with it (248 tests). The failures are `test_check_sloc` (the Rust
ratchet above) and one `test_ccr_wait_idle` case. The 62 stable errors are
`test_ccr_recovery` cases whose `setUp` creates a temporary directory under
`~/.cache/alphaheng`, which this sandbox does not permit.

The 63rd error is `test_ccr_fallback_policy.PolicyTests.test_module_imports_on_the_launchd_system_python`
in another worker's in-flight file. It is a load-sensitive subprocess
timeout, not a deterministic failure: it passes three times out of three when
run alone, and in verbose discovery output it fails at line 32 while the
first `test_ccr_model_benchmark` case starts at line 176, so it runs before the
new test module can affect it. The new test file adds 66 passing tests and no
new failure.

## Non-claims

- No live provider run was performed. Every number in this receipt comes from
  the offline unit tests, a dry run, or the loopback mock.
- The harness does not measure general model quality, reasoning depth, or
  long-horizon agentic behavior. It measures five fixed prompts.
- Latency and throughput from any future live run describe this machine,
  this listener, and this network path, not provider-intrinsic speed.
- A skipped scenario is not a failed scenario. Skipped runs leave the
  accuracy denominator.
- Token counts use upstream usage when reported and a character-based
  estimate otherwise, so estimated tokens make tokens per second approximate.
- The code-fix checker executes model-generated Python in a local subprocess
  with a timeout. That is not a security sandbox.
- No score produced by this harness proves which route is better for a real
  workflow.

## Tier provenance (2026-11-22)

### Scope

Make the per-model benchmark surface tier-fallback contamination instead of
attributing fallback output to the requested model. The transport captures
the two response headers CCR already emits, every run record and every
serialized scorecard run carries them, and the route summary counts the runs
that were served by another provider or that observed rate limited tiers.
No routing, listener, configuration, or service change, and no Rust change.

Owner surface:

- `scripts/ccr_model_benchmark_transport.py` (header capture and parsing)
- `scripts/ccr_model_benchmark.py` (run record, aggregation, summary)
- `scripts/test_ccr_model_benchmark.py` (offline unit tests)
- `docs/observability.md`, `benchmarks/README.md`, `CHANGELOG.md`

### Why this change exists

The live benchmark recorded in `evidence/glm54-model-benchmark-20261123.md`
requested `zai,glm-5.4` and scored 15/15. The listener log showed that three
of those fifteen responses were served by `minimax,MiniMax-M3.1-Flash` after
Z.AI rate limits, and that only 12 of the 15 runs were GLM-5.4 responses.
The scorecard could not show that, because it recorded no tier provenance.

### Fields added

| Field | Where | Meaning |
| --- | --- | --- |
| `served_tier` | `StreamOutcome`, `RunRecord`, every scorecard run | the `x-ccr-tier` header, the display name of the tier that served the request, or `null` |
| `rate_limited_tiers` | `StreamOutcome`, `RunRecord`, every scorecard run | the `x-ccr-rate-limited-tiers` header, split back into the full `provider,model` routes it names |
| `summary.tier_provenance` | every scorecard | the per-route contamination counts |

The scorecard schema stays `ccr-model-benchmark/v1`. The change is additive:
the new run fields default to `null` and `[]`, and the new summary block is a
new key, so an existing reader of the old fields is unaffected.

### Aggregation semantics

`aggregate_runs(runs, route)` adds a `tier_provenance` block. The route is an
optional argument, so the previous one-argument call still works and reports
`requested_provider` as `null` with no mismatch count.

For each attempted run, skipped runs excluded:

- a served tier whose provider component differs from the provider component
  of the requested route counts as a `served_tier_mismatch_runs` run;
- a run whose `rate_limited_tiers` is non-empty counts as a `rate_limited_runs`
  run;
- a run that satisfies either condition counts once in `contaminated_runs`.

The provider component is the part before the first comma, lowercased, so
`zai,glm-5.4` and `zai` both yield `zai`. `x-ccr-tier` carries the tier
display name, which is the provider name unless the provider configures a
custom `tier_name`; such a provider therefore reads as a different provider
than its route id names, and the recorded `served_tier` value is the field to
read in that case.

A run with no served tier is not contaminated. It is counted in
`runs_without_served_tier`, because a missing header is not evidence of a
fallback. `clean` is true only when no run was contaminated and every
attempted run reported its served tier.

`x-ccr-rate-limited-tiers` is only set on a terminal 429, so
`rate_limited_runs` counts terminal rate limit failures, not every tier that
was rate limited during a request that another tier then served. The
served-tier mismatch is what catches that case.

### Evidence

Commands run from the repository root on 2026-11-22.

| Command | Exit | Result |
| --- | --- | --- |
| `python3 -m unittest discover -s scripts -p "test_ccr_model_benchmark.py"` | 0 | 89 tests, OK (66 before this change) |
| `ruff format --check benchmarks scripts tests/provider_validation` | 0 | 25 files already formatted |
| `ruff check benchmarks scripts tests/provider_validation` | 0 | All checks passed |
| `ty check scripts` | 0 | All checks passed |
| `python3 scripts/ccr_model_benchmark.py --models zai,glm-5.4 --models zai,glm-5.3-flash --reps 3 --dry-run --out-dir build/reports/model-benchmark` | 0 | 5 scenarios parsed per route, 1 explicit skip per route, 2 dry-run scorecards written, each with a `summary.tier_provenance` block |
| `python3 scripts/check_sloc.py --json` | 1 | pre-existing: 41,180 Rust nonblank lines against the pinned 41,024 ceiling (this change adds no Rust) |

The new tests cover the header capture on a successful response and on a
terminal 429, the header parsing rules, the rate limited route
reconstruction, the run record plumbing for both the passing and the
failing path, every aggregation count, the scorecard block, and the printed
summary. The transport tests drive a canned response through
`mock.patch.object`, so they need no socket and no listener.

One end-to-end check was run offline with the transport patched to a canned
response whose `x-ccr-tier` was `minimax` while the requested route was
`zai,glm-5.4`. Both runs were attributed to the fallback tier and the
scorecard reported `served_tier_mismatch_runs` 2, `contaminated_runs` 2,
`served_tiers` `{"minimax": 2}`, and `clean` false.

### Non-claims

- No live provider run was performed for this change, and no live listener
  was contacted. The numbers above come from the offline unit tests, a dry
  run, and a patched transport.
- The contamination counts are read from the headers CCR emits. They say
  which tier served a response; they do not prove why that tier served it.
- `rate_limited_runs` counts terminal 429s, because
  `x-ccr-rate-limited-tiers` is only set on a terminal 429. A request that
  fell back after an upstream 429 and then succeeded shows up as a served
  tier mismatch, not as a rate limited run.
- A provider with a custom `tier_name` reads as a different provider than
  its route id names, which over-reports contamination rather than hiding
  it. The recorded `served_tier` value distinguishes the two cases.
- Tier provenance does not change the exit code contract, and it does not
  change how a run passes or fails its checker.

## Live provenance validation (2026-11-23)

The first isolated benchmark run could not validate tier provenance because
its scorecards predate the feature. Two later runs against deployed,
restarted CCR listeners do:

- Worker lane `http://127.0.0.1:3457`, route `zai,glm-5.4`, five scenarios,
  three repetitions: 14/15 passed, `served_tiers` `{"zai": 14, "minimax": 1}`,
  `served_tier_mismatch_runs` 1, `contaminated_runs` 1, `clean` false. The
  contaminated run is the `exact_instruction` repetition 1 failure that
  returned an empty body after Z.AI failed and MiniMax served the request.
- Main lane `http://127.0.0.1:3456`, route `zai,glm-5.4`, scenario
  `vision_two_color`, two repetitions: 2/2 passed, `served_tiers`
  `{"zai": 2}`, `clean` true, no contamination.

Both scorecards carry the `summary.tier_provenance` block with per-run
`served_tier` values. Scorecards:
`build/reports/model-benchmark-live/zai_glm-5.4.json` and
`build/reports/model-benchmark-main/zai_glm-5.4.json`.

This closes the "offline only" limitation for the Responses-protocol Z.AI
route. It does not extend the result to OpenAI-protocol streaming routes,
which do not emit `x-ccr-tier` in this router build and therefore report
unknown provenance.
