# Strong-to-Weak Prefill and Turn-Handoff Assessment — 2026-10-21

## Scope

Literature and architecture assessment evaluating how **strong-model response pre-filling** and **early-turn-to-follow-up model cascading** apply to the CCR-Rust routing and worker-delegation pipeline. This is a read-only engineering handoff document: no router configuration, fallback policy (`scripts/ccr_fallback_policy.py`), runtime source, or listener state was modified.

In CCR-Rust, two-stage model collaboration already operates at the orchestration boundary (`zai,glm-5.3` parent sessions dispatching deterministic briefs to `minimax,MiniMax-M3.1-Flash` and `zai,glm-5.3-flash` on the worker listener) and at the request-routing boundary (`src/router/mod.rs` and `src/gp_router.rs`). The studies reviewed below quantify why front-loading strong-model compute on initial tokens or opening turns works, where it breaks under cross-provider translation, and how assistant pre-filling affects model safety and alignment.

## Literature Summary

| Study | Granularity | Mechanism & Empirical Result | CCR-Rust Pipeline Relevance |
| :--- | :--- | :--- | :--- |
| [Speculative Thinking](https://arxiv.org/abs/2504.12329) (Yang et al., 2025) | Intra-response prefix & reflection handoff | A 32B mentor model pre-fills the first 100 setup tokens and intervenes at `\n\n` reflection delimiters (*"wait"*, *"alternatively"*); lifts a 1.5B model from 83.2% to 89.4% on MATH500 (-15.7% tokens) and a 7B non-reasoning model from 74.0% to 81.8%. | Justifies front-loading structural setup in parent briefs so Flash worker tiers avoid verbose backtracking loops. |
| [The First Few Tokens Are All You Need (UPFT)](https://arxiv.org/abs/2503.02875) (Ji et al., 2025) | Initial prefix tokens (first 8–32 tokens) | Establishes *Prefix Self-Consistency*: valid reasoning trajectories share initial framing steps while errors emerge in later continuation stages. | Explains why strict opening constraints (`<task>`, `<instructions>`, owned file paths) disproportionately stabilize smaller worker models. |
| [Cascade Speculative Drafting](https://arxiv.org/abs/2312.11462) (Chen et al., 2025) & [Faster Cascades](https://arxiv.org/abs/2405.19261) (Narasimhan et al., 2024) | Token-level horizontal cascade | Uses larger models for high-uncertainty opening tokens and smaller models for later, context-constrained tokens via token-level deferral rules. | Relevant only if CCR-Rust adds intra-turn prefix splicing; currently out of scope for single-upstream HTTP dispatch. |
| [DialRouter](https://arxiv.org/abs/2604.12385) (Zhang et al., 2026) | Multi-turn sequential routing | Learns long-horizon MCTS routing policies across dialogue turns; optimal trajectories select stronger models in early turns to anchor context and shift to smaller models on follow-up turns. | Directly maps to turn-aware routing in `src/gp_router.rs` using `dependency_count` (`messages.len() - 1`). |
| [SeqRoute](https://arxiv.org/abs/2605.25424) (Xu et al., 2026) | Session budget-aware routing | Formulates multi-turn routing across strong and weak models as a finite-horizon MDP, cutting session cost by 6.0%–73.5% while suppressing quota exhaustion to <1%. | Complements CCR-Rust's quota/utilization management between `zai,glm-5.3` and `minimax,MiniMax-M3.1-Flash`. |
| [Inter-Cascade](https://arxiv.org/abs/2509.22984) (Wu et al., 2026) | Cross-turn strategy pre-filling | Strong model resolves an initial query and distills a reusable strategy into the weak model's context for subsequent turns (+33.06% weak-model accuracy, -48.05% strong-model calls). | Architectural analog to CCR-Rust's decomposition-first worker brief contract (`AGENTS.md`). |
| [Safety Alignment Should Be Made More Than Just a Few Tokens Deep](https://arxiv.org/abs/2406.05946) (Qi et al., ICLR 2025) | Assistant prefix tokens | Defines *shallow safety alignment*: alignment concentrates on the first few output tokens. Pre-filling an affirmative prefix bypasses refusal; pre-filling a refusal/safe prefix aligns even base models. | Explains both constructive safety anchoring via structured prefixes and the risk of user-supplied `role: "assistant"` pre-fills. |
| [Nudging](https://arxiv.org/abs/2410.09300) (Fei et al., ACL 2025) & [How Does Prefix Matter?](https://arxiv.org/abs/2601.01624) (Tomar et al., 2026) | Discourse & safety prefix anchors | Introductory framing phrases and high-uncertainty discourse markers act as alignment anchors, improving adversarial safety (Safe@1 +6%) and reasoning (+7%) without retraining. | Supports standardized assistant-prefix scaffolding when routing to less-aligned or non-reasoning fallback tiers. |
| [Prefill Awareness in LLMs](https://arxiv.org/abs/2606.12747) (Wang et al., 2026) | Cross-model pre-filled turns | Frontier models detect tampered or foreign assistant turns (up to 35% explicit detection at 0% FPR) via stylistic mismatch and silently revert toward baseline stances under preference mismatch. | Critical risk for cross-provider mid-session handoffs (`zai` $\leftrightarrow$ `minimax`) when assistant history contains provider-specific formatting. |
| [Prefill-level Jailbreak](https://arxiv.org/abs/2504.21038) (Li et al., 2025) & [PRESTO](https://arxiv.org/abs/2512.05518) (Vega & Singh, 2026) | Adversarial assistant pre-filling | User-controlled assistant pre-fills shift first-token probabilities from refusal to compliance (>99% adaptive ASR unless rank/attention regularization is applied). | Identifies an uninspected surface on pass-through endpoints if untrusted callers supply trailing assistant messages. |

## Applicability to the CCR-Rust Pipeline

### 1. Parent-to-worker orchestration (`AGENTS.md`, `docs/worker-inspection.md`)

CCR-Rust's operational split—running interactive planning on `zai,glm-5.3` (`config.json`, port 3456) and delegating bounded work orders via `codex-ccr-worker` to `minimax,MiniMax-M3.1-Flash` -> `zai,glm-5.3-flash` (`glm-workers.json`, port 3457)—is an explicit implementation of the [Inter-Cascade](https://arxiv.org/abs/2509.22984) and [Speculative Thinking](https://arxiv.org/abs/2504.12329) pattern.

Empirical findings from [Speculative Thinking](https://arxiv.org/abs/2504.12329) show that smaller models produce longer outputs on failed trajectories than on correct ones because they enter repetitive self-reflection loops (`\n\n` followed by *"wait"* or *"hmm"*) when initial problem framing is under-specified. CCR-Rust's **Deterministic brief contract** (`AGENTS.md`, enforced by `scripts/test_worker_delegation_docs.py`) directly prevents this failure mode by requiring the stronger parent model to pre-fill the exact owned surface, acceptance commands, and stop conditions before the Flash worker begins decoding.

### 2. Multi-turn sequential routing (`src/gp_router.rs`, `src/router/mod.rs`)

When `strictTierOrder` is `false`, `GpRequestRouter::plan_rerank` (`src/gp_router.rs`) encodes `dependency_count` (`request.messages.len().saturating_sub(1)`), `prompt_chars`, `tool_count`, and `request_class` into the GP surrogate's `FeatureVector` and applies `credible_set_cost_order` to prefer lower-cost tiers inside the quality-credible set.

The multi-turn routing literature ([DialRouter](https://arxiv.org/abs/2604.12385), [SeqRoute](https://arxiv.org/abs/2605.25424)) indicates that turn index and context establishment have asymmetric value:

- **Turn 0 (`dependency_count == 0`)**: Initial architectural framing, plan synthesis, and tool-schema selection benefit heavily from the highest-capability primary tier (`zai,glm-5.3`), because errors in the opening assistant turn compound across the conversation history.
- **Follow-up turns (`dependency_count > 0`)**: Once a high-quality assistant response and tool trajectory exist in `request.messages`, smaller tiers (`minimax,MiniMax-M3.1-Flash` or `zai,glm-5.3-flash`) can execute bounded continuations at substantially lower cost and utilization burn.

### 3. Cross-provider handoff friction (`src/stickiness.rs`, `src/transform/minimax.rs`)

Handing off a conversation from a strong model to a smaller model across different providers introduces three concrete protocol and behavioral hazards that CCR-Rust already partially guards against:

- **Prefill awareness and stylistic reversion**: [Wang et al. (2026)](https://arxiv.org/abs/2606.12747) demonstrate that models detect foreign assistant history via stylistic cues and hidden formatting artifacts, occasionally disavowing prior turns or reverting behavior. CCR-Rust's `minimax` transformer (`src/transform/minimax.rs`), which strips quote-only and control-marker artifacts from assistant history before replay, is essential when a session transitions between `zai` and `minimax`.
- **Thinking-block signature invalidation**: As documented in `docs/claude_code_api_research.md`, Anthropic-compatible `thinking` blocks carry opaque cryptographic `signature` fields, while third-party reasoning models normalize to empty signatures (`signature: ""`). Switching mid-turn or mid-session to a provider that validates native thinking signatures will trigger deterministic 400 rejections (handled once per request by Phase 0 `rejected_tiers` in `src/router/mod.rs`).
- **KV-cache locality loss**: As noted in `src/gp_router.rs` (`estimate_tier_costs`) and `src/stickiness.rs` (`StickySessionTracker`), switching providers on follow-up turns forfeits upstream prompt-cache hits (`cache_read_input_tokens`) on large Claude Code or Codex system prompts. Any cost saved by routing a follow-up turn to a cheaper model must exceed the lost cached-input discount on the primary provider.

### 4. Safety and alignment via response pre-filling

In the alignment literature, response pre-filling is a primary mechanism for both inference-time steering and adversarial jailbreaking:

- **Defensive alignment anchoring**: Because standard safety alignment is concentrated in the first few output tokens ([Qi et al., 2025](https://arxiv.org/abs/2406.05946); [Wang et al., 2026](https://arxiv.org/abs/2603.07445)), pre-pending a short, structured analytical prefix or delegating initial discourse tokens to an aligned model ([Nudging, Fei et al., 2025](https://arxiv.org/abs/2410.09300); [Tomar et al., 2026](https://arxiv.org/abs/2601.01624)) keeps weaker continuation models within safe, task-focused distributions.
- **Ingress prefill-attack surface**: When CCR-Rust exposes `/v1/messages` or `/v1/chat/completions` to external or remote clients (`docs/remote-api.md`), a request whose final message has `role: "assistant"` instructs the upstream model to continue from a caller-supplied pre-fill. Under [Prefill-level Jailbreak](https://arxiv.org/abs/2504.21038) and [PRESTO](https://arxiv.org/abs/2512.05518), affirmative pre-fills bypass shallow refusal guardrails unless the router or upstream strips or inspects trailing assistant pre-fills from untrusted clients.

## Actionable Pilot Options for CCR-Rust

### Option 1: Keep cross-model handoffs at the worker-brief boundary (recommended default)

Retain `strictTierOrder: true` on governed listeners (`scripts/ccr_fallback_policy.py`) and continue enforcing the strong-to-weak split via parent orchestration (`zai,glm-5.3` parent $\rightarrow$ `codex-ccr-worker` on `minimax,MiniMax-M3.1-Flash`). This captures the accuracy and token-reduction benefits of [Speculative Thinking](https://arxiv.org/abs/2504.12329) and [Inter-Cascade](https://arxiv.org/abs/2509.22984) while avoiding cross-provider KV-cache invalidation and thinking-signature mismatches inside a single client transcript.

### Option 2: Opt-in turn-depth feature weighting in `gp_router.rs` (conditional pilot)

For non-strict configurations using `GpRequestRouter`, evaluate an opt-in turn-depth prior where `dependency_count == 0` increases the quality-credible lower bound ($\kappa$ multiplier on Turn 0) so the primary high-capability tier anchors the initial response, while `dependency_count >= 1` relaxes to standard `credible_set_cost_order` so cheaper credible tiers absorb follow-up turns—provided `estimate_tier_costs` accounts for measured `cache_read_per_million_tokens` discounts on the sticky provider.

### Option 3: Trailing-assistant pre-fill policy on remote ingress (optional hardening)

If CCR-Rust is exposed beyond trusted local coding harnesses via `docs/remote-api.md`, add an opt-in request-validation check or transformer that flags or rejects untrusted requests ending with a non-empty `role: "assistant"` pre-fill message, mitigating shallow-alignment pre-fill bypasses ([Qi et al., 2025](https://arxiv.org/abs/2406.05946); [Li et al., 2025](https://arxiv.org/abs/2504.21038)) without altering internal tool-use loops.

## Non-claims

- No live benchmark or A/B routing trial was executed against Z.AI or MiniMax endpoints for this assessment; empirical accuracy and cost figures come from the cited arXiv studies.
- Intra-response token-level handoffs (splicing a 100-token prefix from `zai,glm-5.3` into a second HTTP request to `minimax,MiniMax-M3.1-Flash` within a single `handle_messages` call) are not recommended for CCR-Rust today: double prefill latency, lost KV-cache locality, and streaming SSE translation complexity outweigh the single-turn savings over HTTP APIs.
- No repository source, test, configuration, or governed fallback policy file was changed beyond adding this assessment document.

## References

- Chen, Z., et al. (2025). [Cascade Speculative Drafting for Even Faster LLM Inference](https://arxiv.org/abs/2312.11462). *NeurIPS 2024 / arXiv:2312.11462*.
- Fei, Y., Razeghi, Y., & Singh, S. (2025). [Nudging: Inference-time Alignment of LLMs via Guided Decoding](https://arxiv.org/abs/2410.09300). *ACL 2025 / arXiv:2410.09300*.
- Gupta, N., et al. (2024). [Language Model Cascades: Token-level uncertainty and beyond](https://arxiv.org/abs/2404.10136). *arXiv:2404.10136*.
- Ji, K., et al. (2025). [The First Few Tokens Are All You Need: An Efficient and Effective Unsupervised Prefix Fine-Tuning Method for Reasoning Models](https://arxiv.org/abs/2503.02875). *arXiv:2503.02875*.
- Li, Y., et al. (2025). [Prefill-level Jailbreak: A Black-Box Risk Analysis of Large Language Models](https://arxiv.org/abs/2504.21038). *arXiv:2504.21038*.
- Narasimhan, H., et al. (2024). [Faster Cascades via Speculative Decoding](https://arxiv.org/abs/2405.19261). *arXiv:2405.19261*.
- P S, A., et al. (2024). [Tandem Transformers for Inference Efficient LLMs](https://arxiv.org/abs/2402.08644). *arXiv:2402.08644*.
- Qi, X., et al. (2025). [Safety Alignment Should Be Made More Than Just a Few Tokens Deep](https://arxiv.org/abs/2406.05946). *ICLR 2025 / arXiv:2406.05946*.
- Tomar, R. V., Nakov, P., & Wang, Y. (2026). [How Does Prefix Matter in Reasoning Model Tuning?](https://arxiv.org/abs/2601.01624). *arXiv:2601.01624*.
- Vega, J., & Singh, G. (2026). [Matching Ranks Over Probability Yields Truly Deep Safety Alignment](https://arxiv.org/abs/2512.05518). *arXiv:2512.05518*.
- Wang, A., et al. (2026). [Prefill Awareness in Large Language Models](https://arxiv.org/abs/2606.12747). *arXiv:2606.12747*.
- Wang, G., et al. (2026). [Few Tokens, Big Leverage: Preserving Safety Alignment by Constraining Safety Tokens during Fine-tuning](https://arxiv.org/abs/2603.07445). *KDD 2026 / arXiv:2603.07445*.
- Wu, Y., et al. (2026). [From Deferral to Learning: Online In-Context Knowledge Distillation for LLM Cascades](https://arxiv.org/abs/2509.22984). *arXiv:2509.22984*.
- Xu, Z., Zheng, S., & Wang, W. (2026). [SeqRoute: Global Budget-Aware Sequential LLM Routing via Offline Reinforcement Learning](https://arxiv.org/abs/2605.25424). *arXiv:2605.25424*.
- Yang, W., et al. (2025). [Speculative Thinking: Enhancing Small-Model Reasoning with Large Model Guidance at Inference Time](https://arxiv.org/abs/2504.12329). *arXiv:2504.12329*.
- Zhang, J., et al. (2026). [From Myopic Selection to Long-Horizon Awareness: Sequential LLM Routing for Multi-Turn Dialogue](https://arxiv.org/abs/2604.12385). *arXiv:2604.12385*.
