# DeepSeek Recipe assessment — 2026-10-20

## Scope

Cloned `https://github.com/deepseek-ai/deepseek-recipe` read-only at
`935817c66f5d6e64838f419692545f06787e485f`. The local remote remains fetch-only;
its push URL is disabled. No fork, upstream PR/issue/comment, external push, or
credential use occurred. This was an assessment, not authorization to rewire
CCR routing or add a runtime dependency.

The checkout is MIT-licensed Rust workspace version 0.2.0. It converts Chat
Completions, OpenAI Responses, and Anthropic Messages requests into a
conversation IR, renders DeepSeek V4/V4.1 prompts, optionally encodes/decodes
with bundled tokenizers, and converts caller-supplied inference output back to
those client protocols. The caller still owns inference, tool execution, and
HTTP transport.

## Applicability

### Recommended first adoption: pinned conformance fixtures

Use a small, MIT-attributed subset of upstream request/response vectors as an
offline CCR-Rust conformance corpus. Initial coverage should target reasoning
content, assistant tool calls, tool results, malformed tool shapes, and
JSON-schema output. Keep the vectors copied and pinned in CCR tests with the
upstream revision and copyright notice; do not copy tokenizer assets.

This is useful because CCR already owns cross-protocol translation, while
upstream has a broader declarative corpus for the same three protocol families.
It does not require a runtime dependency or routing change.

### Conditional second pilot: structured-output translation

Upstream validates and maps JSON-schema and regex constraints across protocol
models. CCR currently documents that Messages-to-OpenAI translation maps only
`output_config.effort` and omits `output_config.format`; structured-output
constraints therefore do not survive that path. A purpose-built transformer
could map explicit Anthropic JSON-schema output to OpenAI `response_format`,
but it must be opt-in, reject ambiguous tool/schema conflicts, preserve
Anthropic-native controls, and be proven with mocked upstream request captures.
This is a separate implementation task, not part of this assessment.

### Conditional later option: feature-gated test oracle

If copied fixtures expose broad drift, CCR could add `deepseek-recipe` as a
non-default test-only feature. The default build must remain free of the
dependency. This is premature because the upstream graph includes newer
`jsonschema` and `tokenizers` dependencies and CCR has not yet measured drift.

### Not applicable today

- Do not insert the recipe into CCR's runtime transform chain: it is a
  DeepSeek inference-backend adapter, not a router normalization plugin.
- Do not replace CCR's provider-neutral usage metrics with a DeepSeek
  tokenizer. Exact V4/V4.1 token accounting belongs inside a future local
  DeepSeek inference adapter, not general routing observability.
- Do not transplant raw DSML output parsing into CCR's structured provider
  stream translation. Its parser tests are useful design references, but CCR
  consumes provider events rather than raw local-model text.
- Do not catalog the mock Axum/FastAPI examples or encoding demo as router
  operator tooling. They are useful isolated development harnesses, not health
  or model-serving surfaces.

## Validation

The initial forced-offline attempt passed formatting but could not compile
because the pinned Rust 1.97.1 toolchain and locked `tokenizers 0.23.2` crate
were absent from local caches. It did not establish a source failure.

A network-enabled, task-owned validation then installed hash-verified Rust
1.97.1 components under the checkout's build tree and fetched the locked
dependencies into an isolated Cargo home. Protocol-only validation passed:

- Locked metadata: exit 0.
- Locked fetch: exit 0.
- Format: exit 0.
- Strict Clippy: exit 0 in both `RUSTFLAGS` and explicit `-D warnings` forms.
- Tests: exit 0, 82 passed and 0 failed.
- Strict docs: exit 0.
- Tests with warnings denied: exit 0.

The selected packages were `deepseek-recipe`, `deepseek-recipe-core`,
`deepseek-recipe-encoding`, and `encoding-decoding-demo`. The final tracked
tree remained clean at the pinned revision.

## Worker evidence

Three bounded reviews and two validation runs completed through the GLM
Flash worker route:

- Architecture map `20261020-090515-1vscxfzq`, 259.830 seconds, exit 0,
  private report SHA-256
  `c8aa5e7005e39a7e80782545c8d782d7cb2b86bee1642cdcaf0f7ec9bc602476`.
- Applicability review `20261020-090531-k_bvti_e`, 350.165 seconds, exit 0,
  private report SHA-256
  `62c45c64a2514afeb8c762363270d6c7400289334efcbcff4404ba55d2a41a5a`.
- Adoption review `20261020-090544-hgy5c1ne`, 450.801 seconds, exit 0,
  private report SHA-256
  `c6898a0d7f84dbf36c3f5345a1ac4e9b6a6aa067900c5b286d8f075520c94d26`.
- Initial offline validation `20261020-090601-t4patelc`, 345.377 seconds.
  The launcher process exited 0 after producing its receipt, but the recorded
  validation aggregate was FAILED: formatting passed while compilation and test
  gates were blocked by the missing pinned toolchain and locked crate cache.
- Protocol validation `20261020-091403-fa00mj0t`, 424.277 seconds, exit 0;
  private report SHA-256
  `57b88fe9de0a896c93313701a5ab15703291c4c220c38eab21d784075b965d04`.

## Non-claims

- This assessment does not prove full-workspace compatibility: OpenCV, the
  image crate, Python bindings/wheels, `server-rs`, release builds, packaging,
  and live image/model calls were not validated.
- Passing upstream protocol tests does not prove compatibility with CCR's
  internal APIs or any particular provider.
- No CCR runtime behavior, routing policy, dependency graph, or worker-launcher
  behavior changed in this assessment.
- A future conformance implementation must use pinned MIT-attributed vectors
  and mocked CCR tests; fixture success alone is not live provider acceptance.
