# SenseNova benchmark landscape — 2026-10-23

## Question

Are there broad reviews or benchmarks for SenseNova 6.8 Flash Lite and U1.5
Lite, and how do they compare with models already available locally?

## Short answer

No credible independent, broad benchmark of SenseNova 6.8 Flash Lite or U1.5
Lite was found. SenseTime publishes a vendor comparison chart, but the official
model repository still lists full evaluations as planned. Independent review
sites repeat the vendor positioning or offer editorial estimates without
reproducible scores.

The local fleet has useful performance evidence, but almost none of it is on a
workload shared with SenseNova. There is therefore no defensible fleet-wide
quality ranking from current local evidence. The only matched result is our own
SenseNova non-thinking benchmark and narrow image-extraction/image-generation
probe.

## Public evidence

### Official vendor claims

`sensenova.ai/models` publishes a six-panel comparison chart for SenseNova 6.8
Flash Lite Preview. Reading the chart gives these 6.8 versus 6.7 scores:

| Benchmark | 6.8 Flash Lite Preview | 6.7 Flash Lite |
|---|---:|---:|
| RaccoonPPTBench | 79.60 | 71.20 |
| TeamBench | 44.40 | 32.20 |
| BrowseComp | 77.89 | 47.40 |
| ClawEval pass | 70.60 | 65.80 |
| ArtifactBench | 66.37 | 52.56 |
| Data Analysis 2.0 | 79.10 | 71.00 |

The chart is vendor-produced marketing material. It highlights SenseNova in a
dedicated color, and the footnotes say RaccoonPPTBench and Data Analysis 2.0
are internally developed. The chart shows gains over the predecessor but does
not show 6.8 winning every benchmark; stronger competitors lead several panels.
The scores are self-reported and have not been independently reproduced.

The official `OpenSenseNova/SenseNova6.8` repository says public demo materials
and complete evaluation results "will be added as they become available" and
lists full benchmark results and showcases as planned. That is incompatible
with treating the marketing chart as a complete evaluation package.

The matching U1.5 chart makes vendor claims on Qwen-Image-Bench, IGenBench,
BizGenEval, WeEdit, OmniRef-Bench, and ImgEdit-Bench. It also has no cited
evaluation harness, date, or independent runner. It is useful as a list of the
vendor's intended comparisons, not as independently established quality.

Sources:

- `https://www.sensenova.ai/models`
- `https://github.com/OpenSenseNova/SenseNova6.8`
- `https://github.com/OpenSenseNova/SenseNova6.8/blob/main/README.md`
- `https://www.sensenova.ai/token-plan`

### Independent reviews

Searches over Exa/Jina, OpenCompass, MMBench/OpenVLM, and Hugging Face found no
public independent result for the exact `sensenova-6.8-flash-lite` identifier.
One review site explicitly labels its 8/10 reasoning, 7/10 coding, 9/10 speed,
and 8/10 cost scores as editorial estimates rather than provider-published
ratings, and states that the supplied research contains no verified benchmark
scores. A Chinese launch analysis notes that SenseTime had not published
context capacity, benchmark results, tool success rate, latency, or
task-level cost at the time of release.

The OpenVLM leaderboard includes older SenseNova V6 models, not SenseNova 6.8
Flash Lite. Those older scores say nothing authoritative about the current
preview model.

### Independent evidence for adjacent SenseNova models

Third-party tests do exist for other SenseNova model families, but none is the
hosted 6.8 Flash Lite or U1.5 Lite:

| Third-party benchmark | Model actually tested | Result | Relevance |
|---|---|---|---|
| VistaHop (`arXiv:2606.03273v2`) | SenseNova-MARS-32B | 26.33 Pass@1, barely above GPT-5.2 at 25.83 and far below the 79.50 human score | Shows the agent line is competitive, but it is a 32B model |
| PInVerify (`arXiv:2605.30639v1`) | SenseNova-SI-1.2-InternVL3-8B | 0.833 overall / 0.658 positive | A spatial-intelligence fine-tune, not the hosted Flash Lite |
| EgoP2C (`arXiv:2610.22681v1`) | SenseNova-SI-1.3-Qwen3-VL-8B | +7.5 pp average over base model | Again an SI fine-tune, not the hosted model |
| Spectral Forcing (`arXiv:2606.15236v2`) | SenseNova U1 stage-1 baseline | Wins 9 of 13 DPG-Bench subcategories against its own recipe baseline | Not U1.5 Lite and no peer-model ranking |

OpenRouter's full catalog contained 469 models and zero SenseNova entries.
Hugging Face hosts open-weight U1.5 siblings, but not the hosted 6.8 Flash Lite
endpoint. LMArena, Artificial Analysis, OpenCompass, and FlagEval could not be
read anonymously, so their negative result is not claimed as proof.

## Local evidence inventory

| Model / workload | Existing evidence | Comparable to SenseNova? |
|---|---|---|
| GLM-5.3 Flash, 2,048-token reasoning-heavy generation | median 41.7 tok/s, TTFT 2.67 s, 6 samples | No: generation-span only, max reasoning, different task |
| GLM-5.3 base, same harness | median 43.5 tok/s, TTFT 4.83 s | No: same mismatch |
| GLM-5.3-FlashX | median 122.2 tok/s | No: retired from the current plan and not routable |
| GLM-5.3 traffic-derived | 48.6 tok/s median end-to-end; 37.4 tok/s worker shape | No: traffic, not a controlled benchmark |
| GLM-5.3 vs FlashX, CAD task | P100 fan-adapter 3 repeats, task-scored | No: one CAD task, not a general ranking |
| MiniMax-M3.1-Flash | 4-case fixture-tool suite 2/4; 3-token echo probe | No throughput or capability benchmark |
| DeepSeek Flash | token accounting and failure taxonomy only | No throughput/capability benchmark |
| local27 Qwen3.8-27B MXFP4 | ~115 tok/s prefill and prefix-cache measurements | No decode or task-quality comparison |
| Strata B550 Qwen coder | 140.2 tok/s decode on a different engine revision; 124.4-166.3 tok/s in a later cold arm | No matched CCR-tier benchmark |
| Any local model: image/OCR extraction | none | No |
| Any local model: same 140-word non-thinking task | none | No |

### Local benchmark caveats

- GLM's 41.7 tok/s is first-to-last-chunk generation speed and excludes
  prefill; SenseNova's measured 4.6-5.6 s is inclusive wall time.
- GLM ran thinking at effort max and usually exhausted the cap before visible
  answer text. SenseNova ran with reasoning disabled.
- FlashX is not a current option; it was the fastest tier ever measured locally
  but its trial expired and it is not in the active chain.
- The CAD task is one artifact-generation task. Its own receipt says it does
  not establish a general ranking.
- `openrouter,nvidia/nemotron-3-ultra-550b-a55b:free` returned 404 for image
  input in our comparison, so the "OCR compare" was both rows SenseNova. It is
  not a cross-model vision benchmark.
- local27 has no decode/task-quality benchmark and its backend was not running
  during the route work.
- Strata B550's strongest numbers came from a different engine revision than
  the current CCR tier.

## New matched SenseNova evidence

The 140-word non-thinking task was rerun three times and saved as machine state
in `build/reports/sensenova-nonthinking-text-benchmark.json`:

| Run | TTFT | Wall | Completion tokens | Reasoning tokens | Inclusive tok/s |
|---|---:|---:|---:|---:|---:|
| 1 | 1.124 s | 4.592 s | 170 | 0 | 37.0 |
| 2 | 1.029 s | 5.269 s | 166 | 0 | 31.5 |
| 3 | 1.102 s | 5.553 s | 194 | 0 | 34.9 |

Contrast that with the same model under default thinking: a 140-word task
consumed the full 4,096-token cap on hidden reasoning and emitted no visible
answer. The adaptive/non-thinking override is therefore mandatory for any
throughput comparison.

The image-extraction test is also SenseNova-only: correct four-field JSON,
67-72 completion tokens, zero reasoning, 1.6-3.4 s on the direct-only route.
No local model has been tested on that image.

## Non-claims

- No independent public benchmark or broad review was found; absence of a hit
  is not proof that no private evaluation exists.
- Vendor scores were transcribed from a marketing image and are not
  independently validated.
- No local model was benchmarked on the same text or image task.
- No quota-exhaustion, sustained-concurrency, or tool-heavy benchmark was run.
- The local inventory includes older revisions and different harnesses; it is
  a map of available evidence, not a ranking.
