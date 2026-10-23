# Signed interleaved-thinking replay repair — 2026-10-22

## Scope

Implemented protocol-fidelity repair for signed reasoning across MiniMax
Anthropic responses and Codex/OpenAI Responses tool continuations. The change
also normalizes M3.1 thinking controls and preserves GLM Chat assistant
reasoning history without touching the live GLM native Responses passthrough.

## Implementation

- MiniMax thinking blocks remain structured and hidden. The old
  `[Thinking]` visible-text fallback is gone.
- MiniMax M3.1 normalizes Anthropic-compatible `thinking.type: "enabled"` (and
  drops unsupported budget/display members) to the hosted API's required
  `adaptive` shape. An explicit `disabled` request remains unchanged so the
  provider applies its documented validation.
- MiniMax-supported request `metadata` is no longer stripped.
- Anthropic thinking blocks and signatures serialize into an ordered internal
  `reasoning_blocks` compatibility field.
- Non-streaming Responses output emits a `reasoning` item with
  `reasoning_text` content and opaque `encrypted_content` containing the
  ordered provider blocks.
- Streaming Anthropic `signature_delta` events become an internal
  `reasoning_signature` delta and are emitted on the completed Responses
  reasoning item.
- Subsequent Responses input restores `encrypted_content` to ordered Anthropic
  thinking blocks and groups adjacent reasoning/message/function-call items
  into one assistant turn before the matching tool result.
- The Anthropic tool-normalization round trip preserves signed blocks instead
  of flattening them to unsigned `reasoning_content`.
- OpenAI/Responses upstream dispatch strips the internal `reasoning_blocks`
  carrier after native reasoning conversion; it is not sent as an unsupported
  Chat field.
- The GLM Chat transformer retains Anthropic-shaped assistant thinking blocks;
  the shared OpenAI translation layer converts them once, and GLM-5.3's
  provider-default `max` effort remains unchanged instead of injecting invalid
  `medium`.
- Streaming keeps independent text/signature buffers for each reasoning block,
  so adjacent signatures no longer overwrite one another.
- Structured `encrypted_content` arrays that contain no Anthropic thinking
  blocks are discarded rather than recycled as an opaque signature.

## Verification

- `cargo fmt`
- `cargo test --locked --lib` — 603 tests passed.
- `cargo test --locked --test integration_reasoning_controls` — 15 tests
  passed, including:
  - non-streaming signed thinking plus tool-use Responses round trip;
  - streaming signature-delta Responses round trip;
  - native Anthropic signed-thinking round trip.
- `cargo test --locked --test integration_codex` — 18 tests passed.
- `python3 scripts/check_sloc.py` — passed after splitting test concerns out
  of the Responses and Dispatch hotspot files.
- `RUSTC_WRAPPER=sccache CARGO_INCREMENTAL=0 python3 scripts/quality.py` —
  all 12 offline checks passed.
- `RUSTC_WRAPPER=sccache CARGO_INCREMENTAL=0 cargo build --release --locked`
  and `cargo install --path . --force --locked` — passed; both installed
  executables were replaced. No listener was restarted.
- 2026-10-22 review repairs added focused checks for per-block streamed
  signatures, GLM Anthropic-shaped thinking, all canonicalized M3 aliases,
  strict run timestamp selection, bounded/sanitized salvage commands, and
  malformed structured encrypted reasoning. The lib count is now 605 tests.
  The full review-fix matrix is recorded below after rerunning the gate.

The key mocked end-to-end test proves both requests seen by an Anthropic
provider. The second upstream request contains the exact original signed
thinking block and tool call in one assistant message, followed immediately by
the matching `tool_result`; no hidden reasoning is rendered as visible text.

## Non-claims

- Mocked integration proves CCR's wire shape, not MiniMax's model quality or
  semantic benefit.
- No live MiniMax credential was used and no production listener was restarted.
- Cross-provider signature validity is not claimed. Provider-specific opaque
  state is restored only when a client returns it; a provider that cannot
  validate foreign state remains responsible for rejection.
- The GLM live route continues to use native Responses passthrough; this patch
  deliberately does not add GLM `clear_thinking` controls to that path.
- The delegated read-only protocol-review worker run
  `20261022-124600-2fv0lo5f` executed 108 commands but emitted no review
  verdict. Its receipt remains `task_verified: false`; it is evidence of the
  partial-worker failure mode and is not counted as protocol review approval.
