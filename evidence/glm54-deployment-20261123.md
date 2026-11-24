# GLM-5.4 deployment: 2026-11-23

## Scope

Supersede GLM-5.3 and GLM-5.3 Flash with GLM-5.4 as the Z.AI coding-plan model
across the governed CCR lanes, make GLM-5.4 the Z.AI vision head, and ship the
objective per-model benchmark harness that measures a route on a fixed suite.

## Source state

| Field | Value |
| --- | --- |
| Repository | `/Users/kearm/ccr-rust` |
| Worktree | `.worktrees/glm54-supersede`, branch `feature/glm54-supersede-benchmark` |
| Base snapshot | `51f6490bdc2c634341ed2583965afa3709f4f423` |
| Primary integration | task-owned files copied byte-identically into the dirty primary checkout; no reset, checkout, clean, or replacement of unrelated work |
| Approved policy pin | `b90d82e5dcc12ea370fd11551466e0219f04e6ae4582f0f4863a2d5f0b768104` |

Changed surface: `scripts/ccr_fallback_policy.py`, `scripts/test_ccr_fallback_policy.py`,
`scripts/ccr_codex_catalog.py`, `scripts/test_ccr_codex_catalog.py`, the three
`scripts/ccr_model_benchmark*.py` modules, `scripts/test_ccr_model_benchmark.py`,
the operator and architecture docs, `examples/coding-plans.json`, `CHANGELOG.md`,
and the dated evidence receipts listed below.

## Routing change

- `config.json` and `config-auth.json` primary: `zai,glm-5.4`.
- Shared Z.AI tier: `zai,glm-5.4`, replacing `zai,glm-5.3-flash`.
- `zai` provider registers `glm-5.4`; the superseded ids stay registered but
  unrouted.
- Derived aliases advertise `glm-5.4` and `ccr-primary`; the stale
  `glm-5.3` alias is dropped instead of pointing at a route CCR no longer serves.
- `tetra-vllm` is registered but unrouted: its credential reference, provider
  definition, and prepared tunnel assets are intact, and no generated consumer
  route can dispatch to it. Evidence: `evidence/glm54-tetra-cordon-20261123.md`.

## Deployment actions

| Action | Result |
| --- | --- |
| `ccr-recovery snapshot` | `20261122T221720-0800`, 45 files captured, verified `ok` |
| Deployed policy module | `/Users/kearm/.local/share/ccr-routing/ccr_fallback_policy.py` from reviewed source, mode 0700 |
| Deployed policy JSON | `~/.claude-code-router/fallback-policy.json` regenerated from source constants, hash equals the approved pin |
| `ccr-fallback-policy check` | four consumers reported as changed, no stale staging files |
| `ccr-fallback-policy sync` | `config.json`, `config-auth.json`, `glm-workers.json`, `ocr-reviewers.json` rewritten; every one leads with its GLM-5.4 or OCR primary and carries `zai,glm-5.4` |
| `tetra-vllm` in generated consumers | absent from all four consumer configs |
| Codex client pin | `~/.codex/config-sync/config.toml` `model = "glm-5.4"`, previously `glm-5.3` |
| Worker listener `:3457` | restarted with `ccr-glm-workers-local restart-now`; `/v1/models` advertises `zai,glm-5.4` and `glm-5.4` |
| OCR listener `:3458` | restarted; `/v1/models` advertises `zai,glm-5.4` and `glm-5.4` |
| Auth listener `:3461` | restarted; `/v1/models` advertises `zai,glm-5.4` and `glm-5.4` |
| Main listener `:3456` | restarted with `ccr-local restart-now`; `/v1/models` advertises `zai,glm-5.4` and `glm-5.4`, and a live `glm-5.4` completion returned `GLM54-LIVE-OK` |
| All four governed lanes | `/health` reports `ok`, and no lane advertises `glm-5.3` or `glm-5.3-flash` any more |

The idle drain was skipped on explicit user direction ("we do not need to
worry about everything draining first"). Main and worker were restarted with
the cataloged emergency overrides `ccr-local restart-now` and
`ccr-glm-workers-local restart-now`. OCR and auth have no override wrapper, so
they were restarted with the same `launchctl kickstart -k` that those wrappers
use, after the Arbiter idle gate was explicitly abandoned.

## Live model evidence

`scripts/ccr_model_benchmark.py` against the restarted worker listener
`http://127.0.0.1:3457`, route `zai,glm-5.4`, three repetitions per scenario,
vision declared for the route:

| Metric | Value |
| --- | --- |
| Passed/attempted | 14/15 |
| Accuracy | 0.933 |
| `vision_two_color` | 3/3 passed |
| `code_fix` (executed snippet) | 3/3 passed |
| `strict_json` | 3/3 passed |
| `arithmetic_chain` | 3/3 passed |
| `exact_instruction` | 2/3 passed |
| Median TTFT | 1.371 s |
| Served tiers | `zai` 14, `minimax` 1 |

The single failure is `exact_instruction` repetition 1, which received an
empty body, and the served-tier provenance records that same run as served by
`minimax` after the Z.AI tier failed. The scorecard marks
`summary.tier_provenance.clean` as `false` with `contaminated_runs: 1`. The
GLM-5.4 vision head is therefore live and serving images; the one miss is
attributed to a fallback tier rather than to GLM-5.4.

Scorecard: `build/reports/model-benchmark-live/zai_glm-5.4.json`.

The main lane was then probed separately for vision through the restarted
listener on `http://127.0.0.1:3456`, route `zai,glm-5.4`, scenario
`vision_two_color`, two repetitions: 2/2 passed, median TTFT 1.939 s, served
tiers `zai` 2, `summary.tier_provenance.clean` true, no contamination. The
main lane therefore reads images with GLM-5.4 rather than falling back to
GLM-5.3 Flash. Scorecard:
`build/reports/model-benchmark-main/zai_glm-5.4.json`.

## Clean per-model comparison

The isolated listener on `:3462` was reconfigured with `Router.tiers = []` so
every explicit model dispatches directly and cannot fall back. All four models
stayed registered, and three repetitions per scenario were requested:

| Route | Result | Served tiers | Clean |
| --- | --- | --- | --- |
| `zai,glm-5.4` | 15/15 passed, accuracy 1.000, median TTFT 1.321 s | `zai` 15 | true |
| `zai,glm-5.3` | stopped after provider 429s and 120 s timeouts; no scorecard written | n/a | n/a |
| `zai,glm-5.3-flash` | not run | n/a | n/a |
| `minimax,MiniMax-M3.1-Flash` | not run | n/a | n/a |

GLM-5.4 is the only route that completed a clean, contamination-free
measurement: 15/15 on the fixed suite with every run served by `zai` and
`summary.tier_provenance.clean` true. The comparison was then stopped because
GLM-5.3 was answering with HTTP 429 `rate_limited` and 120-second timeouts in
the same window, which would have burned the rate-limited plan without
producing comparable evidence. The earlier mixed-tier run in
`evidence/glm54-model-benchmark-20261123.md` remains the only four-route
comparison, and its contamination caveat still applies to it.

Scorecard: `build/reports/model-benchmark-clean/zai_glm-5.4.json`.

## Preflight deviation

`ccr-fallback-policy preflight --json` returned `ok: false` for three
pre-existing downstream tiers, unchanged by this work:

- `deepseek`: no probe endpoint answered.
- `local27`: connection refused on the resident MLX proxy.
- `strata-b550`: connection reset by the B550 tunnel.

`zai`, `minimax`, `meta`, `openrouter`, `holo4`, and `sensenova-vision` all
authenticated. The user directed an immediate rollout without waiting for
drains, so the pre-existing downstream failures were recorded and the rollout
proceeded. They affect fallback depth only: the GLM-5.4 primary itself is
qualified and live.

## Catalog activation is still blocked

`ccr-catalog-sync --check` fails with `source model metadata is absent:
glm-5.4`. The Z.AI model listing does not publish a GLM-5.4 metadata entry, and
the builder deliberately refuses to clone GLM-5.3's context window and
modalities onto a slug nobody measured. Codex therefore runs `glm-5.4` with
generic metadata, which is the same posture the machine already used for
`glm-5.3`, because `~/.codex/ccr-model-catalog.json` never carried a GLM-5.3
entry either. Routing is unaffected; only Codex model-picker metadata is.

## Non-claims

- This receipt does not claim GLM-5.4 is better than another model. It records
  a fixed five-scenario suite on one machine and network path.
- It does not claim the benchmark measures general model quality, reasoning
  depth, or long-horizon agentic behavior.
- It does not claim the pre-existing DeepSeek, local27, or Strata failures were
  repaired, and it does not claim Tetra is healthy or unhealthy today.
- It does not claim the Codex catalog now carries GLM-5.4 metadata.

## Evidence index

- `evidence/glm54-vision-qualification-20261123.md` - direct Z.AI vision probe.
- `evidence/glm54-model-benchmark-20261123.md` - first isolated benchmark run,
  retained because its three MiniMax fallbacks motivated tier provenance.
- `evidence/model-benchmark-harness-20261122.md` - harness design and metrics.
- `evidence/glm54-tetra-cordon-20261123.md` - Tetra cordon rationale.
