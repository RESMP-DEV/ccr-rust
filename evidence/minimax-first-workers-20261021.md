# MiniMax-first scoped workers, 2026-10-21

## Scope

Source-only change to the governed CCR fallback policy: reorder the shared
route chain so MiniMax M3.1 Flash is the scoped-worker primary and Z.AI GLM
5.3 Flash becomes the final shared fallback. User authorization is dated
2026-10-21; the motivation is that the Z.AI plan keeps exhausting its monthly
utilization window, so GLM Flash should not be the first tier a scoped worker
consumes.

This change does not deploy, restart listeners, or touch anything under
`~/.claude-code-router` or `~/.local/share`.

## Exact source changes

- `scripts/ccr_fallback_policy.py`: reordered `MACHINE_ROUTE_CHAIN` from
  `("zai,glm-5.3-flash", "minimax,MiniMax-M3.1-Flash")` to
  `("minimax,MiniMax-M3.1-Flash", "zai,glm-5.3-flash")`; changed
  `MACHINE_CONSUMERS["glm-workers.json"]` from `"zai,glm-5.3-flash"` to
  `"minimax,MiniMax-M3.1-Flash"`; updated the source comments to record the
  2026-10-21 rationale; recomputed `MACHINE_POLICY_INTEGRITY_SHA256` to
  `d90d53f21e76a4723fb9128eaa0b1e7f47baa5ea0d0bc430fe3fea5cc666b16f`.
- `scripts/test_ccr_fallback_policy.py`: updated `test_minimax_worker_primary_uses_the_official_non_preview_model`,
  `test_removed_tiers_stay_registered_providers`,
  `test_glm_flash_is_final_fallback_and_main_auth_primary_unchanged`,
  `test_worker_chain_leads_with_minimax_and_keeps_glm_flash_fallback`, and
  `test_main_and_auth_share_the_chain_after_the_qwen_removal` to assert the
  new order and worker primary.
- `docs/shared-fallback-policy.md`: replaced the "GLM-5.3-Flash followed by
  MiniMax" paragraph with the MiniMax-first order, the 2026-10-21
  authorization date, and the utilization-window rationale.
- `CHANGELOG.md`: added the dated routing-behavior entry under `[Unreleased]`.

## Commands and exit codes

```
Parent integrated-tree validation, 2026-10-21:
uv run --no-project python -m unittest discover -s scripts -p 'test_ccr_fallback_*.py'
  Ran 36 tests in 7.028s; OK; exit 0
RUSTC_WRAPPER=sccache CARGO_INCREMENTAL=0 python3 scripts/quality.py
  12/12 checks PASS; exit 0; receipt build/reports/quality/receipt.json

PolicyTests (29 tests): exit 0, all pass
  uv run --no-project python -c "... loadTestsFromTestCase(PolicyTests) ..."
  tests=29 failures=0 errors=0

Full acceptance command (both test files):
  uv run --no-project python -m unittest discover -s scripts -p 'test_ccr_fallback_*.py'
  Ran 36 tests: 26 pass, 10 errors (all PermissionError from socket.bind)
  exit 1 in this sandbox; the 10 errors are entirely sandbox socket-binding
  restrictions on PreflightTests and RuntimeTests, not assertion failures.
  The 5 PreflightTests and RuntimeTests use ThreadingHTTPServer on 127.0.0.1:0,
  which the CCR sandbox denies with "[Errno 1] Operation not permitted".
```

## Resulting tier orders (verified via derive)

```
glm-workers.json:    minimax,MiniMax-M3.1-Flash -> zai,glm-5.3-flash
config.json:         zai,glm-5.3 -> minimax,MiniMax-M3.1-Flash -> zai,glm-5.3-flash
config-auth.json:    zai,glm-5.3 -> minimax,MiniMax-M3.1-Flash -> zai,glm-5.3-flash
ocr-reviewers.json:  openrouter,nvidia/nemotron-3-ultra-550b-a55b:free
                     -> minimax,MiniMax-M3.1-Flash -> zai,glm-5.3-flash
```

The OCR order was verified with a locally defined `openrouter` provider stub
because that provider is defined in the actual consumer config, not in
`MACHINE_SHARED_PROVIDERS`.

## Parent deployment, 2026-10-21

The parent owns live-service activation and performed it after the integrated
quality gate passed.

- Backed up the deployed module to
  `~/.local/share/ccr-routing/ccr_fallback_policy.py.bak-20261021-pre-minimax-first`
  and the policy JSON to
  `~/.claude-code-router/fallback-policy.json.bak-20261021-pre-minimax-first`.
- Reordered the live policy JSON and installed the reviewed module. The
  repository, central routing copy, and service-root symlink target all had
  SHA-256 `5ff49bf078216fa67aae0bb70da81a9fac165fcc24a9020b1b3cbf285eed348d`.
  Stale `ccr_fallback_policy*.pyc` entries were removed from the three service
  import roots.
- `ccr-fallback-policy preflight --json` — exit 0. Z.AI and MiniMax both
  authenticated with HTTP 200 model listings; GMI, DeepSeek, and Qwen stayed
  `registered_not_routed`; policy SHA-256 was
  `d90d53f21e76a4723fb9128eaa0b1e7f47baa5ea0d0bc430fe3fea5cc666b16f`.
- `ccr-fallback-policy check --json` before synchronization — expected exit 1,
  with all four consumers changed. `ccr-fallback-policy sync --json` then
  exited 0, rewrote all four consumers, took the recovery pre-write snapshot,
  and reported no stale staging files. A post-sync `check` exited 0 with no
  changed consumers.
- Idle-gated `ccr-recovery activate` restarts exited 0 for main (port 3456;
  idle after 23.705 seconds), auth (port 3461; idle after 0.514 seconds), and
  OCR (port 3458; idle after 136.747 seconds). Their live
  `/health?details=1` defaults and tier orders now match the derived policy.
- The worker listener (port 3457) remained busy with other sessions' live
  workers, so its first idle-gated activation correctly stopped at the idle
  stage after 900 seconds rather than interrupting them. No worker-listener
  restart has been forced. Its on-disk config is synchronized; new launcher
  processes already request `minimax,MiniMax-M3.1-Flash` from it.
- Live process evidence at 10:46 PDT showed four in-flight `codex exec`
  workers on the MiniMax route (`tune-env`, `tune-env`, `tune-env`, and the
  MLX worktree) and one pre-change GLM worker still draining. A completed
  launcher receipt, run `20261021-104002-l06x_76t`, recorded
  `model_requested=minimax,MiniMax-M3.1-Flash`, outcome `completed`, exit 0,
  and 73 completed commands.
- A representative non-streaming Responses completion explicitly requested
  `minimax,MiniMax-M3.1-Flash`: HTTP 200, `model=MiniMax-M3.1-Flash`,
  `status=completed`, and output `MINIMAX_FIRST_OK`. The worker listener's
  MiniMax request counter rose from 2804 to 2806 around that completion.

## Non-claims

- The worker listener process had not yet drained enough for an idle-gated
  restart at receipt time. Its runtime `/health?details=1` still showed the
  old default and tier order, although its config and every newly launched
  worker route selection were already MiniMax-first.
- The worker-sandbox PreflightTests/RuntimeTests socket errors were
  environmental; the parent reran both focused suites and the full quality
  matrix outside that sandbox with green results.
