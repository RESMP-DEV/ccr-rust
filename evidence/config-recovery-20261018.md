# CCR configuration recovery and Codex model metadata, 2026-10-18

## Scope

Two operator-visible defects in the live CCR surface on this machine, plus the
recovery capability that makes the first class of damage non-fatal.

1. A CCR routing change had no reliable rollback. The only recovery artifacts
   were ad hoc dated copies beside the live files, one of which was empty
   (`~/.claude-code-router/backups/20260917T201355/`), and the deployed policy
   writer took no snapshot before rewriting four consumer configs.
2. Codex sessions pinned to the `ccr-primary` alias warned "Model metadata for
   `ccr-primary` not found. Defaulting to fallback metadata" on every turn,
   because the local model catalog never gained an entry for the alias the
   2026-10-18 route restore had just introduced.

## Recovery tooling

`scripts/ccr_recovery.py` (installed as `ccr-recovery`, plus copies beside both
policy execution copies so the pre-write hook can import it)
captures 28 files across six areas: the four consumer configs, the policy JSON,
the deployed policy module, credential files, seven CCR launchd plists, the
operator wrappers and the files behind their symlinks, the tunnel configs, and
the Codex catalog plus active `config.toml`.

Design points that are load-bearing:

- Snapshots are staged and moved into place only after the manifest is written,
  so an interrupted run cannot leave a directory that looks valid.
- `verify` re-hashes every file; restore refuses a snapshot that does not verify.
- `restore` is a dry run unless `--apply`. Apply stages beside the destination,
  fsyncs, then replaces, then re-parses the consumer configs, and only restarts
  listeners behind the `ccr-wait-idle` gate.
- The manifest records whether a captured path was a symlink, because
  re-deriving that from live state cannot work during a restore.
- Snapshot roots are mode `0700` and captured files keep their own modes,
  because some live files carry raw API keys.
- The Codex profile and role pins are captured alongside the catalog, because a
  restored catalog alone cannot recover a session launcher that points at a
  retired route.

`sync` and `serve.py` now call `pre_snapshot` before any write, so an ordinary
routing change captures the prior state with no operator step. `sync --check`
stays read-only and takes no snapshot.

Two live hygiene defects fixed alongside: `com.kearm.ccr-cordon-activate`
(a launchd job that waited for idleness and then re-kickstarted the main
listener with a stale MiniMax-only chain) was booted out, its plist retained on
disk; and every world-readable secret-bearing file under
`~/.claude-code-router` (`.env` plus dated `*.bak*` copies, four of which held
literal provider keys) was tightened to `0600`.

## Policy integrity pin

`load_policy` already gated the live policy on route order, consumer
registrations, and provider definitions. An agent editing
`fallback-policy.json` directly could still change a field those checks do not
constrain, so a whole-body SHA-256 pin (`MACHINE_POLICY_INTEGRITY_SHA256`)
was added after the structural checks. Changing the policy body now requires a
source change with tests and review.

## Fallback activation

Live log evidence explained the observed GLM-to-MiniMax delays. Main listener
3456 had started at 11:22 UTC with the then-current in-memory order
`zai,glm-5.3 -> zai,glm-5.3-flash -> minimax,MiniMax-M3.1-Flash-Preview`;
the later synchronized config order is
`zai,glm-5.3 -> minimax,MiniMax-M3.1-Flash-Preview -> zai,glm-5.3-flash`.
Thus a GLM 429 first paid a GLM Flash attempt before MiniMax. Around 12:35 UTC
the logs also prove that once both GLM tiers returned 429, MiniMax completed
successfully repeatedly; the cascade itself worked, but main had stale order.

Worker, auth, and OCR listeners were idle-restarted onto the repaired binary.
Main could not be restarted while this and other sessions held active requests.
`com.kearm.ccr-idle-restart-once` is therefore waiting under launchd for two
consecutive zero-request/zero-stream samples, will restart main onto the
synchronized order, and then unloads itself. The waiter uses a six-hour idle
timeout and does not bypass the gate.

## OCR post-update status

At 14:48 UTC the OCR webhook and listener were healthy, with seven active
router requests. Since OCR process 71663 started at 12:40:38 UTC, five active
reviewer profiles produced successful reviewed markers: Gemini 3.8 Flash (3),
GLM 5.3 (1), Inkling (2), MiniMax M3.1 Flash Preview (2), and Nemotron 3 Ultra
(2). The router also recorded 363 successful MiniMax, 207 successful Z.AI, and
105 successful OpenRouter upstream responses in that window.

This is not full-model coverage. Laguna had 208 requests but no successful
reviewed marker after the restart; its current review remained running behind
persistent OpenRouter 429/request failures. The configured
`zai,glm-5.3-flash` fallback tier had no attempt in the current OCR process.
OCR still returned the legacy plain-text `/health` body because its idle-gated
restart remained blocked by active traffic, so runtime doctor correctly stayed
failed for OCR.

Follow-up hardening closed the activation gap that incident exposed. `/health`
now reports the in-memory default and ordered tiers; `ccr-recovery doctor`
compares all four live listeners to their consumer configs and exits nonzero on
stale routes even when each listener is HTTP-healthy. Restore's `--restart`
path includes OCR, and snapshots capture `serve.py`,
`serve-glm-workers.py`, `shape_probe_auth.py`, and both copies of the recovery,
catalog, and idle-gate tool manifests.
`ccr-recovery activate --listener <name>` is the durable single-listener verb
for delayed activation; it reuses the same two-sample idle gate and health
check. Self-unloading launchd waiters are currently draining main and OCR.

## Model metadata

`~/.codex/ccr-live-model-catalog.json` gained a `ccr-primary` entry cloned from
the verified `glm-5.3` entry (same context window, xhigh defaults, reasoning
levels, and turn-contract `base_instructions`), with `ccr-primary` as
slug/id/model. It is written atomically at mode `0600`. The durable
`ccr-catalog-sync` command now derives metadata for `ccr-primary`, both GLM
Flash route spellings, and both exact MiniMax preview spellings from the GLM
source, removes retired FlashX entries, rejects duplicate slugs, and snapshots
before writing.

Codex loads `model_catalog_json` into an in-memory `StaticModelsManager` at
startup and does not re-read the file, so the running daemon still served the
old catalog (`model/list` returned only the four pre-existing slugs after the
file changed). For already-running threads the fix is
`thread/settings/update`, the same request the TUI itself uses to switch model
in a live session; two threads (`01a14ec3-…`, the ccr-rust session, and
`01a14ec8-…` in loc-infra) were switched from `ccr-primary` to `glm-5.3`, which
is catalog-backed and resolves to the same primary tier through the router's
alias. Future sessions can use `ccr-primary` directly.

## Evidence

- `python3 -m unittest discover -s scripts -p 'test_*.py'` from
  `/Users/kearm/ccr-rust` — 84 tests, OK (32 for the recovery tool, including
  snapshot verification, symlink restore, scoped restore, secret-mode
  preservation, dry-run default, corrupt-snapshot refusal, and the two
  symlink-drift cases in `doctor`). `uv run --no-project ruff check` clean on
  every file this change touched.
- Live corruption-and-recovery proof: `config.json` was overwritten with
  `{"broken": true}`; `ccr-recovery doctor` reported drift; the dry run showed
  2 drifted entries without writing; `ccr-recovery restore --apply` restored
  26 files, re-parsed the configs, and `doctor` returned clean with the restored
  `Router.default` of `zai,glm-5.3`.
- `ccr-recovery snapshot --label post-metadata-fix` — 28 files, manifest
  verified; `ccr-recovery doctor` — ok, 30 snapshots retained, no drift.
- `ccr-recovery sanitize` — 25 files copied, and `.env`,
  `ccr-api-key.env`, and `runtime-credentials.json` omitted from the
  replication view (confirmed absent on disk afterwards).
- Deployed parity: `sha256 b8d71772…` identical across the repo source and the
  three execution copies of `ccr_recovery.py`; `sha256 64bec04f…` identical
  across source and both copies of `ccr_fallback_policy.py`.
- Metadata proof: `codex exec -m ccr-primary --sandbox read-only --json`
  completed with exit 0, the agent reply "CCR metadata OK", and no
  `Model metadata … not found` warning in stdout or stderr. The
  `thread/settings/update` calls returned `{}` (the documented empty success
  response) and `thread/read` then reported `glm-5.3` for both threads.
- Routing after the switch: a live `/v1/chat/completions` with `model=glm-5.3`
  returned content `ok` with `model=glm-5.3`; the router log shows a 429 on
  `zai,glm-5.3` followed by "Success on zai after 2.28s (attempt 2)", i.e. the
  cascade worked as designed.
- Catalog proof: `ccr-catalog-sync --check` reports no changes or errors after
  removing both FlashX entries and adding `glm-5.3-flash`,
  `zai,glm-5.3-flash`, `MiniMax-M3.1-Flash-Preview`, and
  `minimax,MiniMax-M3.1-Flash-Preview`. Live `codex exec` probes for all four
  route spellings exited 0, emitted their exact `META_OK_<slug>` replies, and
  contained no metadata warning or error.
- Profile proof: `ccr.config.toml`,
  `claude-code-router.config.toml`, and `ccr-roles/glm-worker.toml` now pin
  `ccr-primary`; a focused search found no `glm-5.3-flashx` pin in those three
  files.
- `ah register ccr-catalog-sync --json` registered the launcher;
  `ah doctor --probe` reports `probe=ok` for both `ccr-catalog-sync` and
  `ccr-recovery`.
- Fresh-idle regression repair: a restarted listener initially returned an
  empty Prometheus exposition because both idle gauges were lazily registered
  only after first use. `/metrics` now materializes both gauges, the dedicated
  fresh-process test passes, and worker 3457, auth router 3461, and OCR 3458
  all report `ccr_active_requests 0` and `ccr_active_streams 0` after restart.
  Worker and OCR restarts were idle-gated; auth was idle by direct gauge
  inspection because its proxy's pre-fix exposition lacked the stream gauge.
- Installed router SHA-256 `a1972d4e…` matches `target/release/ccr-rust`.
- Replication posture: Syncthing has a new send-only `ccr-recovery-shared`
  folder pointed at `~/.local/share/ccr-recovery/shared` and no folder pointed
  at the private snapshot root. The final sanitized snapshot copies 29 routing
  files plus its manifest and omits `.env`, `ccr-api-key.env`,
  `runtime-credentials.json`, and Syncthing `config.xml`.
- Final quality after the Syncthing recovery-scope change: `python3
  scripts/quality.py` passed format, clippy, Rust tests, no-default-features,
  Python helpers, and vendored GP-routing. Final snapshot
  `20261018T055450-0700` contains 34 private files, verifies, has no drift or
  missing live target, and sanitizes to 31 files with all four secret-bearing
  files omitted.
- Runtime activation proof: `/health` returns the in-memory default and ordered
  tiers. `ccr-recovery doctor` now marks main, worker, and auth runtime-clean;
  OCR remains runtime-stale only because its idle gate stayed busy. The
  deployed router SHA-256 is now `2d3d152f…`, identical to
  `target/release/ccr-rust`.
- Latest quality and snapshot: after adding runtime route checks, OCR restart,
  launcher/tool capture, and the single-listener `activate` verb,
  `python3 scripts/quality.py` passed all six checks. Snapshot
  `20261018T073959-0700` verifies 43 private files with zero file drift and
  sanitizes to 40 files while omitting `.env`, `ccr-api-key.env`,
  `runtime-credentials.json`, and Syncthing `config.xml`.
- `launchctl bootout gui/501/com.kearm.ccr-cordon-activate` — exit 0; the label
  no longer appears in `launchctl list`; the plist remains on disk.
- `find ~/.claude-code-router -maxdepth 1 -type f \( -name '.env' -o -name
  '*.env' -o -name '*credential*' -o -name '*.bak*' -o -name '*.pending' \)
  -perm -004` — no output after the chmod pass.

## Non-claims

- A restored snapshot reproduces the state it captured; nothing here proves
  that state was correct when it was taken.
- Restore is a filesystem operation. It does not contact providers and does not
  prove a credential still authenticates.
- `doctor` compares the newest snapshot against live files; it cannot detect a
  change that was made and then reverted.
- Route metadata is cloned from `glm-5.3`, including for the MiniMax preview
  tier. This preserves Codex tool/context/compaction tuning, but it does not
  measure MiniMax-specific context limits; update the source entry when a
  provider-accurate catalog is available.
- `ccr-recovery` was not exercised against `--restart` on a busy listener; the
  idle gate path is covered by unit tests with mocked gates only.
- OCR 3458 still runs the previous `/health` contract because its gate stayed
  busy; a six-hour self-unloading launchd waiter is active. Runtime doctor
  correctly remains failed until OCR drains, restarts, and reports its loaded
  tier order.

## 2026-10-19 rebase note

The recovery branch was rebased after the MiniMax non-preview promotion and the
Qwen DeepSeek dual-ID routing change. The historical preview observations above
remain unchanged; current catalog generation adds
`MiniMax-M3.1-Flash`, both Qwen 3.8 Flash/Max route spellings, and both dated
and undated Qwen DeepSeek V4.1 Flash routes. Runtime drift checks now request
`/health?details=1`, preserving the content-free `/health` liveness contract
while detecting listeners too old to expose routing details.

Pre-PR review and hardening completed on 2026-10-19:

- An independent read-only CCR worker (GLM-5.3 Flash) found four valid issues:
  retained MiniMax Preview catalog slugs, unsafe manifest paths, an overbroad
  “secret-free” replication claim, and malformed catalog entries causing an
  unstructured crash. The branch now retires both Preview slugs, validates
  manifest paths/records/symlink targets before restore or replication, fails
  closed on credential-shaped content before creating a share, and reports
  malformed catalog entries through JSON errors.
- Focused tests: 28 fallback-policy tests, 44 recovery tests, 6 catalog tests,
  and the fresh-process Prometheus gauge test all pass. `ruff check` passes on
  every touched Python file.
- Live read-only compatibility: all 30 existing recovery snapshots verify under
  the stricter manifest parser; `ccr-recovery doctor` reports no file drift,
  no missing live target, and runtime-matching main/worker/auth/OCR listeners.
  The newest snapshot sanitizes 39 files while omitting all four known
  secret-bearing filenames and passing the content scan.
- Final canonical matrix at `build/reports/quality/receipt.json` passed format,
  clippy, Rust tests, no-default-features, Python helpers, and vendored GP
  routing on the repaired source.
