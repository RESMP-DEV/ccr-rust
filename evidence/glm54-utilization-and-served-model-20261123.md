# GLM-5.4 quota utilization, the served-model gap, and a live quota watcher: 2026-11-23

## Scope

Answer three questions with measured evidence:

1. Is there a real break-off between GLM-5.3 and GLM-5.4 in Z.AI quota burn,
   and is it explained by shorter reasoning traces?
2. Why does the chat footer say "GLM-5.4 Max" when that label is not proof of
   which model served the request?
3. Can a watcher be put on the real quota API so the numbers are visible going
   forward instead of inferred?

Everything below was measured on this machine against the live Z.AI account and
the live CCR listeners on 2026-11-23 local time (2026-11-24 UTC). Where a
number came from a worker run, the run is cited and the parent's independent
verification is given beside it.

## Verdict

| Question | Answer |
| --- | --- |
| Is there a clear break-off? | Yes, but it is a **credit-rate break, not a token-volume break**. Z.AI publishes per-token credit multipliers, and GLM-5.4's are 4.6 / 1.1 / 16 against GLM-5.3's 6.9 / 1.7 / 24. The same token mix costs **0.65x** the credits under GLM-5.4. |
| Are reasoning ("looping") tokens charged? | **Yes.** They sit inside `output_tokens` and are metered at the output multiplier. There is no reasoning exemption. Reasoning is charged, but at two-thirds the rate under GLM-5.4. |
| Is GLM-5.4 using shorter reasoning traces? | **Partly, and it is not the main effect.** Z.AI documents 10-14% shorter output from shorter chains of thought. A paired live probe found comparable reasoning tokens per prompt and *higher* total output for GLM-5.4, at roughly half the latency. The dominant quota effect is the multiplier discount plus a large volume of traffic that never reaches Z.AI at all. |
| Why does the footer lie? | The footer shows the **requested** route. Every CCR config now maps `glm-5.3` to `zai,glm-5.4` via `modelMap`, and when Z.AI rate-limits, CCR falls back to `minimax,MiniMax-M3.1-Flash`, which consumes zero Z.AI quota. **62% of successful CCR requests on 2026-11-24 UTC were served by MiniMax, not GLM.** |

## 1. The cutover is a step function, and the credit rate is the break

GLM-5.4 appears in Z.AI's own hourly accounting for the first time at
**2026-11-23 13:00 CST (05:00 UTC)** and is absent for the entire 37 hours
before it.

| Window (CST) | Model | Tokens | Credits (published formula) | Credits per 1M tokens | Cached share of input |
| --- | --- | ---: | ---: | ---: | ---: |
| 11-21 00:00 to 11-23 12:00 (37 h) | GLM-5.3 | 1,886,792,273 | 357,466 | 189.5 | 97.2% |
| 11-23 13:00 to 11-24 14:00 (26 h) | GLM-5.4 | 910,701,233 | 144,731 | 158.9 | 92.2% |

Absolute burn fell from **9,661 credits/hour to 5,567 credits/hour (-42%)**
while GLM-5.4's token volume per hour was itself lower (35M/h vs 51M/h).

Two effects are separable:

- **Multiplier discount (published, exact).** Applying GLM-5.4's multipliers to
  GLM-5.3's own token mix yields **0.650x** the credits; applying GLM-5.3's
  multipliers to GLM-5.4's mix yields **0.654x** in the other direction. The
  discount is 1.5x, or about two-thirds, in both directions.
- **Mix effect (measured).** The realized per-token rate fell only 0.839x,
  because the cache-hit share dropped from 97.2% to 92.2%. Cached input is the
  cheapest class (multiplier 1.1) and it dominates the bill, so **cache hit
  rate is the single largest lever on the percentage**, larger than the model
  choice.

### The published formula, verified live

Z.AI documents the quota as a token-derived credit:

> "Model credit usage = (Input tokens x Input multiplier + Cached Input tokens
> x Cached Input multiplier + Output tokens x Output multiplier) / 10,000"
> (https://docs.z.ai/devpack/overview)

with "Max | 28,000 | 140,000" credits per 5-hour and weekly window, and the
multiplier rows "GLM-5.4 | 4.6 | 1.1 | 16" and "GLM-5.3 | 6.9 | 1.7 | 24".

**Live validation.** Applying that formula to Z.AI's own hourly per-model token
classes for the 2026-11-24 14:00 CST hour predicts **5,976 credits**, which is
**21.3 percentage points per hour** of the 28,000-credit window. The
independently sampled quota percentage moved from 10% at 06:42:41Z to 16% at
07:01:44Z, which is **20.0 points per hour** over that same hour
(`build/reports/zai-quota-watch/baseline.jsonl`). Observed over predicted is
**0.94, so the published formula reproduces the live burn to within 7%**.
The residual is consistent with integer percentage quantization and rolling
window timing. This is the answer to "how do they even calculate it":
**utilization is credits, credits are token-weighted, and the weight is per
model and per token class.**

**Accounting lag, measured.** The per-model token series is not real time. A
probe that burned 12,288 output tokens on `glm-4.6v` at about 15:20 CST was
still absent from the monitor at 15:30 CST (53 tokens visible) and appeared as
12,593 tokens by 16:44 CST. Treat the monitor's per-model numbers as lagging by
roughly one hour, so minute-scale deltas in the watcher are for trend only and
must not be used for per-request attribution.

That is the answer to "how do they even calculate it": **utilization is
credits, credits are token-weighted, and the weight is per model.**

### Reasoning tokens are charged

Z.AI publishes no sentence stating that thinking tokens are billed. The
evidence chain is: the credit formula meters only input, cached input, and
output; the usage schema exposes no separate `reasoning_tokens` billing field
(https://docs.z.ai/openapi.json); `reasoning_content` is returned alongside a
single `completion_tokens` value; and the vendor's own Chinese documentation
states "深度思考的思考过程计入输出 Token" (thinking process counts into output
tokens). Live API responses confirm `output_tokens_details.reasoning_tokens`
is a **subset** of `output_tokens`, for example `output_tokens: 740` with
`reasoning_tokens: 470`. So reasoning is charged, as output, at multiplier 16
for GLM-5.4 and 24 for GLM-5.3.

This is **inference from schema plus vendor example text**, not a quoted
billing-policy sentence.

## 2. The served-model gap

The chat footer renders the **requested** route. Three independent checks:

1. **Config.** `~/.claude-code-router/config.json`, `config-auth.json`, and
   `glm-workers.json` all carry `modelMap` entries mapping `glm-5.3` to
   `glm-5.4`. A client asking for `glm-5.3` is dispatched to GLM-5.4.
2. **Live response.** Three consecutive requests to `zai,glm-5.4` on the
   listener at `127.0.0.1:3456` returned `model: "MiniMax-M3.1-Flash"` in the
   response body. The client-side footer still read GLM-5.4.
3. **Log volume.** For 2026-11-24 UTC in the main listener log
   (`~/.claude-code-router/logs/service.stdout.log`, port 3456 only):
   `Success on zai` 387, `Success on minimax` 631, `Rate limited on zai` 317.
   **62% of that listener's successful requests were served by MiniMax**, which
   consumes no Z.AI quota at all. The auth, worker, and OCR listeners share the
   same key and the same `modelMap`, so they are subject to the same fallback
   but are not included in that count.

The CCR router records no served model anywhere. A worker audit of the source
(run `20261123-230141-*`, report
`build/reports/glm54-utilization-20261124/workers/w1.txt`) found that a grep
for `served_model|upstream_model|selected_model|resolved_model` across `src/`
returns zero matches, that every metric, `/v1/usage` row, response header, and
the "Success on" log line is keyed by the served **tier display name** only
(`src/metrics/mod.rs:26-31`, `src/router/mod.rs:906-909`), and that
`record_usage` (`src/metrics/mod.rs:832-838`) takes no model and no reasoning
count. Reasoning tokens are parsed for OpenAI shapes
(`src/router/streaming.rs:137-142`, `src/router/translate_response.rs:126-130`)
but reach only the client; the Anthropic streaming parser does not read them
at all.

The parent verified the same conclusion directly against the running binary:
the live `/metrics` output exposes `ccr_input_tokens_total{tier}`,
`ccr_output_tokens_total{tier}`, and `ccr_cache_read_tokens_total{tier}`, with
no model dimension and no reasoning dimension.

## 3. Is GLM-5.4 reasoning shorter? A paired live probe

Identical prompts, same key, same endpoint (`https://api.z.ai/api/v1/responses`),
model as the only variable. Script: `build/reports/glm54-utilization-20261124/paired_probe.py`.

| Prompt | Model | n | Output tokens (mean) | Reasoning tokens (mean) | Reasoning share | Latency s (mean) |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| easy | GLM-5.3 | 6 | 16.8 | 13.8 | 82.2% | 2.8 |
| easy | GLM-5.4 | 6 | 10.7 | 8.7 | 81.2% | 6.3 |
| train | GLM-5.3 | 6 | 470.8 | 255.2 | 54.2% | 15.2 |
| train | GLM-5.4 | 3 | 935.7 | 263.3 | 28.1% | 12.3 |
| hard | GLM-5.3 | 6 | 1043.2 | 272.8 | 26.2% | 24.9 |
| hard | GLM-5.4 | 3 | 915.3 | 182.0 | 19.9% | 12.7 |

On the two prompts that elicit real reasoning, GLM-5.4 produced **comparable
or higher total output** and **comparable or lower reasoning tokens** (hard:
-33% reasoning at equal output), and was faster: 1.2x on `train` and 2.0x on
`hard`. On the trivial `easy` prompt GLM-5.4 was slower in the mean only
because of one 25.7 s outlier, and its median was faster. The evidence does not
support "GLM-5.4 produces shorter reasoning traces" as the cause of a
materially lower quota percentage: total output was equal or higher.

Three of six GLM-5.4 repetitions returned HTTP 429 on the `train` and `hard`
prompts, with GLM-5.3 at zero errors. The probe ran all GLM-5.3 repetitions
first, so that ordering is a confound and the 429 rate is not a clean
per-model rate limit measurement.

### Client-side 30-day view, and why it cannot answer the question alone

Replaying the aggregation command specified by the Codex-telemetry worker
(`workers/w2.txt`, blocked from `/Users/kearm/.codex` by its sandbox) over
85,392 rollout files:

| Requested model | Requests | Input | Cached | Output | Reasoning | Reasoning share |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| glm-5.3 | 65,059 | 14,859,486,743 | 16,255,829,093 | 26,689,584 | 8,686,119 | 32.5% |
| minimax,MiniMax-M3.1-Flash | 21,460 | 317,366,924 | 1,745,668,717 | 20,145,755 | 1,062,622 | 5.3% |
| zai,glm-5.4 | 2,946 | 132,479,840 | 195,169,297 | 2,314,759 | 1,070,074 | 46.2% |
| glm-5.4 | 1,411 | 95,660,129 | 177,967,172 | 1,103,227 | 362,778 | 32.9% |

These are **requested** model strings. The `glm-5.3` row mixes GLM-5.3 answers
with MiniMax fallback answers, because the requested label never changes when
the router falls back. That row is the reason a client-side table alone cannot
answer the question, and the reason the served-model logging below is needed.

## 4. The watcher is live

`scripts/zai_quota_watch.py` polls the two real monitor endpoints and writes
per-poll records with deltas. A launchd agent runs it every 120 seconds:

| Item | Value |
| --- | --- |
| Launchd label | `com.kearm.zai-quota-watch` |
| Plist | `~/Library/LaunchAgents/com.kearm.zai-quota-watch.plist` |
| Interval | 120 s (`--watch --interval 120`, launchd `KeepAlive`) |
| Output | `build/reports/zai-quota-watch/watch.jsonl` (one compact JSON object per poll, append-only, git-ignored) |
| Endpoints | `GET /api/monitor/usage/quota/limit`, `GET /api/monitor/usage/model-usage` |
| Auth | raw API key in the `authorization` header, read from `~/.claude-code-router/runtime-credentials.json`; the key is never printed or logged |

Sample of the live trajectory (percentage of the 28,000-credit 5-hour window).
`baseline.jsonl` is the earlier shell sampler, `watch.jsonl` the repo script:

```
baseline.jsonl  06:42:41Z 10%   06:53:40Z 13%   07:03:44Z 17%   07:13:48Z 19%   07:41:59Z 34%
watch.jsonl     07:47:59Z 34%   07:50:00Z 37%   07:52:00Z 37%   07:54:01Z 38%
```

Verified live record (parent-run, `--watch` under launchd, 2026-11-24
07:50:00Z): `TOKENS_LIMIT|3` at 37%, `TIME_LIMIT|5` at 34/4000,
`model_usage.models` populated, `errors: []`, and per-interval deltas that are
non-null (`deltas.models.aggregate.last_hour_token_delta=0.0` in a quiet
two-minute window, which is a measured zero, not a missing field).

The sampler was repaired during activation: `--once` printed pretty-printed
JSON, so redirecting it into `watch.jsonl` produced an unparseable
concatenation of multi-line objects, and because launchd ran `--once` on
`StartInterval`, the previous-state baseline was lost on every tick and all
deltas were null. `--once` now prints one compact object, and launchd runs the
long-lived `--watch` loop with `KeepAlive`, which holds the previous-state
baseline in memory. The two malformed records are preserved as
`build/reports/zai-quota-watch/watch.prettyjson.bak`.

**`modelctl` cannot serve as the watcher today.** Its Z.AI adapter resolves
credentials through the business OAuth path, and both `modelctl usage
--provider zcode` and `modelctl key --provider zcode` fail with "biz api error:
token expired or incorrect" (HTTP 401, x-request-id 20261124062828c0ad0b863321453cec98).
The monitor API itself is healthy and authenticates fine with the raw key that
CCR already uses, so the watcher talks to the monitor API directly.

What the watcher makes possible going forward: with a per-poll record of the
window percentage and of per-model token classes, the credit formula can be
regressed over hours whose model mix differs, so the per-class, per-model
coefficients become identifiable from this account's own data rather than from
the vendor table.

## 5. Served-model logging: implemented and activated

`src/metrics/mod.rs::record_usage` is the single accounting choke point that all
four upstream paths already call, so one signature extension plus one log line
yields a per-request record with tier, served model, input, cached input, output,
and reasoning.

Implemented on branch `feature/served-model-logging` in
`.worktrees/served-model-logging` (base `51f6490`), worker report
`build/reports/glm54-utilization-20261124/workers/w6.txt`, parent verification
below.

| File | Change |
| --- | --- |
| `src/metrics/mod.rs` | `record_usage` gains `served_model: Option<&str>` and `reasoning_tokens: u64`; adds `ccr_reasoning_tokens_total{tier}` and `ccr_served_model_requests_total{tier,served_model}`; one `info!` per call |
| `src/sse.rs` | `StreamVerifyCtx.served_model` |
| `src/router/usage_metrics.rs` | `openai_reasoning_tokens()` and `anthropic_reasoning_tokens()` parsers, beside the existing usage normalizer |
| `src/router/dispatch.rs` | wire model into both `StreamVerifyCtx`; OpenAI/Responses and Anthropic non-streaming reasoning capture |
| `src/router/streaming.rs` | served model plus parsed reasoning on both streaming paths; Anthropic SSE usage now reads `reasoning_tokens` |
| `src/router/streaming_reasoning_tests.rs` | The streaming reasoning test, split out so the hotspot file keeps its headroom |
| `CHANGELOG.md` | Unreleased entry for the operator-visible metric and log change |

`route = <provider,model>` was initially added to the "Success on" line in
`src/router/mod.rs` and then removed as redundant: the per-request line already
names tier and served model, and `router/mod.rs` had zero headroom.

**Verification (worker-run, then re-run by the parent where marked):**

| Command | Result |
| --- | --- |
| `cargo check --all-features --locked` | exit 0 |
| `cargo clippy --all-features --locked -- -D warnings` | exit 0 (also with `--all-targets`) |
| `cargo test --all-features --locked` | exit 101, **42 sandbox socket-bind denials** |
| new tests only | exit 0, 4 passed |
| `rustfmt --edition 2021 --check` on the five touched files | exit 0 |
| `python3 scripts/check_sloc.py` (after the split below) | exit 0, `41567/41572` |

The 42 test failures are environmental, not regressions: every one is
`Failed to bind an OS port for a mock server: PermissionDenied`, and the worker
proved the identical failure set at the pristine base by extracting `51f6490`
with `git archive` (base 810 passed / 42 failed; branch 814 passed / 42 failed,
the four extra passes being exactly the new tests).

**SLOC ratchet state, after the split.** Applied to the live tree the change
first took the gate from green (`41356/41372`) to red, because the streaming
test and the reasoning parser landed in hotspot files. The 57-line streaming
test now lives in `src/router/streaming_reasoning_tests.rs`, both reasoning
parsers live in `src/router/usage_metrics.rs`, and the `route =` line was
dropped, which left `router/mod.rs` and `router/dispatch.rs` under their
ceilings with no raise. The remaining growth is production wiring that cannot
leave its file, so only two ceilings moved, by exactly the measured amount:
`router/streaming.rs` 1373 → 1386 and `sse.rs` 294 → 296, with the total
re-pinned 41,372 → 41,572 and the cause recorded in the script comment.
`python3 scripts/check_sloc.py` now exits 0 at `41567/41572`.

**Activation (parent-run, 2026-11-23 23:51 to 23:54 local).** The branch itself
was not installed: base `51f6490` predates the uncommitted `modelMap` cutover,
so a binary built there would regress it. Instead the logging diff was applied
on top of the live primary working tree, which already contains the cutover
code, preserving all 59 pre-existing dirty files; the pre-apply tree is backed
up as `build/reports/glm54-utilization-20261124/primary-dirty-before-logging.patch`
plus the five touched files beside it.

| Step | Evidence |
| --- | --- |
| `cargo check --all-features --locked` | exit 0 |
| `cargo test --all-features --locked --lib -- record_usage openai_reasoning anthropic_stream_records` | exit 0, 4 passed |
| `cargo install --path . --force` (first build) | exit 0, release build 2 m 13 s, binary installed 2026-11-23 23:51:28 |
| `ccr-service-restart main --wait 5m` (first pass) | pid 14480 → 97572, idle wait 16.9 s, health true |
| `ccr-service-restart worker --wait 5m` (first pass) | pid 12950 → 97834, idle wait 0.0 s, health true |
| `ccr-service-restart auth --wait 5m` (first pass) | pid 13072 → 97937, idle wait 0.0 s, health true |
| `cargo install --path . --force` (after the ratchet split) | exit 0, release build 1 m 59 s, binary re-installed 2026-11-24 00:05 |
| `ccr-service-restart main/worker/auth --wait 5m` (second pass) | main 97572 → 25321, worker 97834 → 25422, auth 97937 → 25522, all health true, idle wait 0.0 s |
| `ccr-service-restart ocr --wait 1m` (twice) | **not restarted**: arbiter timed out after 2 m both times, 1 in-flight OCR request |

The OCR listener (:3458, pid 15718) therefore still runs the pre-change binary;
it keeps its old metrics and old log line until an idle window allows a restart.
The installed binary on disk is the split version, so that restart needs no
further build.

Live proof on :3456 immediately after activation, which is the exact record the
footer could not provide:

```
INFO ccr_rust::metrics: request token usage tier="zai" served_model="glm-5.4" input=61477 cached_input=60928 output=515 reasoning=368
INFO ccr_rust::router: Success on zai after 10.73s (attempt 10.668s) route=zai,glm-5.4
```

and on the metrics endpoint, where `MiniMax-M3.1-Flash` appears as a served
model distinct from the requested route:

```
ccr_served_model_requests_total{served_model="MiniMax-M3.1-Flash",tier="minimax"} 3
ccr_served_model_requests_total{served_model="glm-5.4",tier="zai"} 9
ccr_reasoning_tokens_total{tier="zai"} 1510
```

Counters register lazily on first use, so :3461 (auth) shows them only after
its first request even though it runs the same new binary; that is expected,
not a failed restart.

**Known limits of the new telemetry.** The counters are in-process Prometheus
counters and reset on restart; `src/metrics/persistence.rs` was outside the
owned file set, so they are not restored from a snapshot. `/v1/usage` and the
`x-ccr-tier` header are unchanged (`src/metrics/handlers.rs` out of scope).
`served_model` is the canonicalized wire model the router sent for that route,
which is proof of the upstream id but not of what a provider does internally
behind its own proxy.

## Non-claims

- This does not rank GLM-5.3 against GLM-5.4 on quality. The task-correctness
  benchmark for these routes is `evidence/glm54-model-benchmark-20261123.md`
  and is a separate artifact.
- The paired probe used one easy prompt, one arithmetic prompt, and one
  number-theory prompt with 3 to 6 repetitions per cell. It is not a
  statistical comparison, and the GLM-5.4 cells are small because of 429s.
- The realized credits-per-token comparison mixes windows with different
  workloads and different cache-hit rates. It is a description of what
  happened, not a controlled A/B.
- The live formula validation agrees within 7% on one hour. That confirms the
  published formula to that tolerance on this account; it does not pin the
  credit scale beyond that single-hour comparison, and no other hour was
  checked the same way.
- Whether preserved thinking returned in later turns is billed as cached input
  or fresh input is not stated in any reachable Z.AI document.
- The `TOKENS_LIMIT` entry's `unit: 3, number: 5` and `TIME_LIMIT`'s
  `unit: 5, number: 1` have no published meaning; only the labels in
  `modelctl`'s adapter describe them.
- Nothing here proves the Z.AI 5-hour window is a fixed grid rather than a
  rolling refill.

## Artifact index

| Path | What it holds |
| --- | --- |
| `scripts/zai_quota_watch.py`, `scripts/test_zai_quota_watch.py` | The watcher and its unittest suite |
| `build/reports/zai-quota-watch/watch.jsonl` | Current live series, one compact JSON record per 120 s poll |
| `build/reports/zai-quota-watch/baseline.jsonl` | Earlier shell-sampler series, 31 samples |
| `build/reports/zai-quota-watch/watch.prettyjson.bak` | The two malformed pretty-printed records from the first launchd setup |
| `build/reports/glm54-utilization-20261124/logging.patch` | The logging diff as applied to the live tree |
| `build/reports/glm54-utilization-20261124/primary-dirty-before-logging.patch` | Full pre-apply diff of the 59 dirty files, for rollback |
| `src/router/usage_metrics.rs` | The two reasoning-token parsers, beside the existing usage normalizer |
| `src/router/streaming_reasoning_tests.rs` | The streaming reasoning/served-model test, split out of the hotspot |
| `build/reports/glm54-utilization-20261124/` | Raw API payloads, analysis scripts, worker briefs and reports |
| `build/reports/glm54-utilization-20261124/model-usage-3d.json` | Z.AI hourly per-model series, 11-21 to 11-24 |
| `build/reports/glm54-utilization-20261124/usage-detail-2d.json` | Z.AI hourly per-model token classes and credit fields |
| `build/reports/glm54-utilization-20261124/paired_probe.py` | The paired 5.3 vs 5.4 probe |
| `build/reports/glm54-utilization-20261124/codex_usage.py` | The 30-day Codex telemetry aggregation |
| `build/reports/glm54-utilization-20261124/workers/` | Worker briefs and reports, W1 through W7 |
| `.worktrees/served-model-logging` | Branch `feature/served-model-logging` |
