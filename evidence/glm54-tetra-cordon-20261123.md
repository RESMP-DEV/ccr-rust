# GLM-5.4 rollout Tetra cordon: 2026-11-23

## Decision

The GLM-5.4 rollout removes `tetra-vllm,deepseek-ai/DeepSeek-V4.1-Flash` from
`MACHINE_ROUTE_CHAIN` while leaving the provider registered. No generated
consumer route, provider entry, or credential reference is emitted while the
route is absent. The prepared tunnel assets remain unchanged for a future
explicit activation.

This cordon exists because the inherited source route is not deploy-complete:
`CCR_TETRA_VLLM_API_KEY` is unresolved, the tunnel LaunchAgent is not installed,
local port 18317 has no listener, and Tetra-side port 8317 is closed. Live Tetra
host actions remain user-authorization-gated.

## Restored policy shape

- `zai,glm-5.4` remains the main/auth primary and shared Z.AI tier.
- The active chain ends at `local27,qwen3.8-27b-mxfp4`.
- `tetra-vllm` remains in `MACHINE_SHARED_PROVIDERS`.
- Future activation requires explicit host authorization, the named SSH tunnel,
  credential resolution, and a live endpoint health check in one reviewed change.

## Non-claims

This cordon does not invalidate or rewrite the historical 2026-11-13 Tetra
qualification. It does not prove Tetra is permanently unavailable, repair its
tunnel, create or rotate a credential, or authorize any Tetra host action.

## Evidence

The recomputed approved policy integrity pin is
`b90d82e5dcc12ea370fd11551466e0219f04e6ae4582f0f4863a2d5f0b768104`.
Focused policy and deployment verification for this rollout are recorded in
`evidence/glm54-deployment-20261123.md`.
