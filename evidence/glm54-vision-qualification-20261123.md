# GLM-5.4 vision qualification — 2026-11-23

## Scope

Qualified GLM-5.4 as the Z.AI vision-capable model before it replaced
GLM-5.3 and GLM-5.3 Flash in the governed routing policy. The probe ran
against the exact endpoint CCR dispatches to for the `zai` shared provider
(`https://api.z.ai/api/v1`, protocol `responses`, route `POST /responses`),
using the repository's own two-color PNG procedure documented in
`docs/zai_anthropic_endpoint.md`. The credential was read from
`~/.claude-code-router/runtime-credentials.json` and never printed.

## Live probe results

| Model | Probe | Result |
|---|---|---|
| glm-5.4 | text only | HTTP 200, `text='ready'`, usage input tokens 19 |
| glm-5.4 | image, left half red, right half blue | HTTP 200, `left=red, right=blue`, usage input tokens 79 |
| glm-5.4 | image, left half green, right half yellow | HTTP 200, `left=green, right=yellow` |
| glm-5.3-flash (control) | image, left half red, right half blue | HTTP 200, `left=red, right=blue` |
| glm-5.3 (control) | image, left half red, right half blue | HTTP 200, `I can't see any image content here` |

The input-token inflation from 19 to 79 on the image request confirms the
PNG was processed rather than dropped by the transport, and the second
green/yellow probe rules out a lucky guess on one color pair. GLM-5.3 is
the blindness control: it answered the identical image request with a
text-only refusal, so the correct GLM-5.4 answers are a property of the
model and not of the probe harness.

## What this authorizes

GLM-5.4 serves text and reads a two-color PNG correctly, so it can carry
both roles that the 2026-11-23 user direction assigned to it: the canonical
coding-plan primary and the Z.AI vision head. The source supersession in
`scripts/ccr_fallback_policy.py` moves the `config.json` and
`config-auth.json` primaries to `zai,glm-5.4`, moves the shared Z.AI chain
tier from `zai,glm-5.3-flash` to `zai,glm-5.4`, and registers `glm-5.4` in
the `zai` provider model list. The superseded ids (`glm-5.3`,
`glm-5.3-flash`) stay registered but unrouted, like `gmi`, `qwen`, and
`deepseek`, so a dated re-approval restores them without rewiring
credentials.

## Non-claims and open items

- The probe ran against Z.AI directly, not through a CCR listener, because
  no live configuration carried GLM-5.4 at the time. CCR-side wire
  compatibility is by construction, not measurement: CCR converts an
  Anthropic `image` block to an OpenAI `image_url`
  (`src/router/translate_request.rs:32`) and then to a Responses
  `input_image` (`src/router/responses_protocol.rs:67-77`), which is the
  body the successful probe used. The end-to-end check through
  `127.0.0.1:3456` is a deployment step, not a completed measurement.
- Z.AI's authoritative capability surface does not expose GLM-5.4. The
  coding-plan listing reports only `glm-5.3`, `glm-5.3-flash`, and
  `glm-5-turbo`, so the modality evidence here is empirical, not
  catalog-backed. The platform listing
  (`GET https://api.z.ai/api/paas/v4/models`) does name `glm-5.4` but
  carries no capability fields and reports an unfunded balance for this key.
- Unmeasured for GLM-5.4: context window, reasoning levels, and tool-call
  behavior. The Codex catalog builder therefore refuses to synthesize a
  GLM-5.4 profile from GLM-5.3's entry; the live catalog needs a real
  `glm-5.4` entry before `ccr-catalog-sync` can run.
- A two-color PNG proves image transport and color reading, not general OCR
  quality.
- GLM-5.4 shares the plan's rate-limit bucket and intermittently answered
  HTTP 429 `rate_limit_exceeded` under the live load observed during the
  audit, so the shared Z.AI tier is now a single model rather than a
  GLM-5.3-Flash backstop inside the same plan.

## Source of the evidence

Read-only CCR worker audit run `20261122-210158-fm4i_nzv`, final report at
`~/.cache/ccr-worker-runs/20261122-210158-fm4i_nzv/final.txt`, brief at
`~/.cache/ccr-worker-runs/20261122-210158-fm4i_nzv/brief.txt`. The run
changed no live state.
