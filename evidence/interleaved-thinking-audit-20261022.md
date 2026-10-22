# Interleaved-thinking and reasoning-replay audit — 2026-10-22

## Scope

Read-only audit of official GLM-5.3, MiniMax M3.1 Flash, DeepSeek, Anthropic,
and OpenAI Responses reasoning guidance, compared with CCR's live protocol
paths. No production route, transformer, client configuration, or listener was
changed.

## Official verdicts

### GLM-5.3 / GLM-5.3 Flash

Z.AI documents interleaved thinking as enabled by default: GLM can reason
between tool calls and after tool results. Tool-use history must preserve the
model's reasoning and return it with tool results. Preserved Thinking requires
the complete, unmodified, correctly ordered `reasoning_content`; the Coding
Plan enables this by default, while the standard Chat API needs
`thinking.clear_thinking: false`. GLM-5.3 native effort levels remain
`low`, `high`, and `max`; it cannot disable thinking.

The live CCR GLM provider is native Responses at `https://api.z.ai/api/v1`
with no transformer. A local mock-router probe sent a Responses history
containing `reasoning`, `function_call`, and `function_call_output` items. CCR
passed the complete input array, including the reasoning item, unchanged to the
Responses upstream. Therefore the generic Responses parser's separate
Chat-conversion path is not the active GLM history path. A worker claim that
the live GLM route drops reasoning input was rejected by this parent-run probe.

### MiniMax M3.1 Flash

MiniMax officially documents native Interleaved Thinking. Its guide says to
return the full response every tool round, especially thinking fields. On the
Anthropic path this means replaying the complete ordered `response.content`
list containing `thinking`, `text`, and `tool_use` blocks. Thinking blocks
carry an opaque `signature` that must be returned unchanged. M3.1-Flash always
thinks, accepts adaptive thinking, rejects `disabled`/`none`, and supports
`output_config.effort` values `low`, `medium`, `high`, `xhigh`, and `max`
(default `max`). It may omit thinking for a simple tool call, so a missing
thinking block is valid and must not be synthesized.

CCR's active MiniMax provider is Anthropic protocol with the `minimax`
transformer, while Codex workers present an OpenAI Responses frontend. A
mock-router two-round probe reproduced four concrete contract violations when
MiniMax returned `[signed thinking, tool_use]`:

1. The transformer converted the thinking text into visible output text
   `[Thinking]\n...` because it only checked for a text block, not `tool_use`.
2. The Codex serialization collapsed structured thinking into a plain
   `reasoning_content` summary and dropped the MiniMax signature.
3. On the next request, the generic Responses-to-Anthropic conversion ignored
   the prior `reasoning` input item.
4. The assistant turn was split into separate visible-text and tool-call
   messages instead of one ordered content list.

The second upstream request therefore received visible text plus `tool_use`,
but no MiniMax thinking block or signature. That breaks the official replay
contract and is the highest-value repair candidate. A live direct MiniMax tool
probe also produced a valid no-thinking tool call, confirming that absent
thinking must remain absent rather than being fabricated.

Native Anthropic Messages clients are not affected in the same way: CCR has a
passing integration test that preserves MiniMax's ordered signed thinking and
tool blocks across a native Messages round trip.

### DeepSeek Flash

Current DeepSeek docs say that when tools are present every prior
`reasoning_content` must be returned; omitted reasoning can produce HTTP 400.
Its Anthropic compatibility surface accepts thinking blocks. CCR's generic
Responses-to-Anthropic history conversion has the same latent reasoning-loss
risk on the DeepSeek tier. This was not live-reproduced here.

## Prompting and reasoning-trace augmentations

Safe and documented:

- Preserve provider reasoning items/blocks exactly and in order; do not edit,
  reorder, summarize, or synthesize them.
- Keep tool results immediately associated with their tool calls; Anthropic
  requires `tool_result` blocks first in the following user message.
- Use native effort controls rather than asking for visible step-by-step
  reasoning. GLM-5.3 uses `low/high/max`; MiniMax uses
  `low/medium/high/xhigh/max`.
- Structure agent briefs with goal, context, constraints, and a done-when;
  both Z.AI and MiniMax officially recommend this shape.
- Define each tool's purpose, inputs, return shape, failure behavior, and
  parallel-versus-sequential rule. MiniMax documents this directly.
- Use explicit stopping rules and avoid over-eager tool loops.
- Keep plans/checklists as agent-level artifacts or tools, not synthetic API
  thinking blocks.

Unsafe or unsupported here:

- Do not inject a fabricated thinking block when MiniMax deliberately omitted
  one.
- Do not expose raw reasoning as normal assistant text; use the provider's
  reasoning field/block.
- Do not replay a provider-specific signature after switching providers.
- Do not send `thinking.type: disabled`, `effort: none`, or GLM-5.3 `medium`.
- Do not add GLM `clear_thinking` to the native Responses path unless Z.AI
  documents that field for Responses; the Coding Plan already enables Preserved
  Thinking by default.
- Do not put architecture, plan, or reasoning instructions in a router
  transformer. Those belong in the client/parent prompt; the router should
  preserve transport fidelity.

## Evidence and probes

Official sources:

- Z.AI Thinking Mode: `https://docs.z.ai/guides/capabilities/thinking-mode.md`
- Z.AI GLM-5.3: `https://docs.z.ai/guides/llm/glm-5.3.md`
- Z.AI coding best practices:
  `https://docs.z.ai/devpack/resources/best-practice`
- MiniMax Tool Use & Interleaved Thinking:
  `https://platform.minimax.io/docs/guides/text-m3-function-call`
- MiniMax Anthropic API:
  `https://platform.minimax.io/docs/api-reference/text-anthropic-api`
- MiniMax prompting:
  `https://platform.minimax.io/docs/token-plan/prompting-best-practices`
- DeepSeek Thinking Mode:
  `https://api-docs.deepseek.com/guides/thinking_mode/`
- OpenAI reasoning-items guidance:
  `https://developers.openai.com/api/docs/guides/reasoning`

Local artifacts:

- Mock protocol comparison:
  `build/reports/interleaved-thinking-audit/ccr-replay-probe.json`
- MiniMax signed-thinking round trip:
  `build/reports/interleaved-thinking-audit/minimax-roundtrip-probe.json`
- Live MiniMax no-thinking tool call:
  `build/reports/interleaved-thinking-audit/minimax-live-tool-response.json`
- Worker reports:
  `build/reports/interleaved-thinking-audit/{minimax,prompting,glm-resumed}.txt`

Read-only worker run IDs:

- MiniMax contract audit: `20261022-114456-r1nu6tob`
- prompting survey: `20261022-114517-gn4eb_ss`
- GLM audit initially failed after collecting evidence:
  `20261022-114436-gn1r3uxt`
- GLM resumed report-only completion:
  `20261022-115449-18u39r4f`

## Recommended repair shape, not applied

1. First remove the MiniMax visible `[Thinking]` fallback when `tool_use` or
   another usable block survives; add regression coverage for signed
   thinking plus tool use.
2. Carry an opaque provider signature through the Responses reasoning item
   (for example, `encrypted_content`) and restore it to the Anthropic thinking
   block on replay.
3. Group adjacent Responses `reasoning`, `message`, and `function_call` items
   into one ordered Anthropic assistant turn, followed immediately by matching
   `tool_result` blocks.
4. Keep GLM native Responses passthrough unchanged.
5. Add non-streaming and streaming MiniMax/DeepSeek round-trip tests before
   changing production routing.

## Non-claims

- No authenticated GLM or DeepSeek reasoning round trip was run.
- The live MiniMax tool call demonstrates a valid no-thinking response, not a
  signed-thinking loss; that loss was reproduced against a mock Anthropic
  upstream using the same CCR protocol and transformer path.
- Worker reports are evidence, not acceptance. The GLM worker's claim that the
   live GLM Responses route drops reasoning items was rejected by the parent's
   mock passthrough probe.
- The local Strata and `local27` Qwen crawl-back tiers were not audited for
  interleaved thinking. Strata currently forces `reasoning_effort: none`, so
  interleaved reasoning is intentionally disabled there.
- No code or production configuration was changed by this audit.
