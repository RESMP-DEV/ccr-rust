# Hardened listener activation — 2026-10-22

## Scope

The main CCR listener was activated onto the freshly installed Rust binary from
PR #73. Recovery activation was then hardened so future restarts distinguish a
healthy long-lived process from a process actually executing the replacement
executable.

## Live activation evidence

- Pre-activation snapshot:
  `20261022T135642-0700` (`pre-main-listener-activation-20261022`), 42 files,
  verified with no missing, mismatched, or unexpected files.
- The self-unloading main idle-restart LaunchAgent waited for two idle samples,
  then restarted the listener.
- Final main process: PID `35989`, started 2026-10-22 13:59:21 PDT, listening
  on `127.0.0.1:3456`.
- Process text mapping and installed binary both resolve to vnode inode
  `913490734` at `~/.cargo/bin/ccr-rust`.
- `/health?details=1` returned the configured GLM-5.3 default and six-tier
  order.
- A low-effort live completion returned content `OK`, finish reason `stop`,
  0 reasoning tokens, and 23 total tokens. No model error was returned.
- `ccr-recovery doctor`, `ccr-catalog-sync --check`, and the main config
  validation passed after activation.

## Hardening implementation

Activation now:

1. captures a verified, complete pre-activation snapshot and refuses to restart
   if verification fails or any inventory target is missing;
2. uses the two-sample idle gate;
3. restarts the governed listener;
4. compares the process text vnode inode with the installed Rust binary;
5. checks runtime default and tier order against its consumer config; and
6. optionally makes a tiny low-effort completion with `--live-check`.

Snapshots also capture both `~/.local/bin/ccr-recovery` and the deployed
`ccr_recovery.py`, so the recovery implementation is part of its own rollback
surface rather than being reconstructed from a stale copy.

## Validation

- `ruff format scripts/ccr_recovery.py scripts/test_ccr_recovery.py`
- `ruff check scripts/ccr_recovery.py scripts/test_ccr_recovery.py`
- `PYTHONPATH=scripts python3 -m unittest discover -s scripts -p test_ccr_recovery.py`
  — 60 tests passed after review repairs for alternate snapshot roots,
  malformed completion choices, and scalar finish-reason output
- `ty check scripts`
- `RUSTC_WRAPPER=sccache CARGO_INCREMENTAL=0 python3 scripts/quality.py` — all
  12 checks passed after review repairs.

## LaunchAgent interpreter follow-up

The first worker and OCR delayed-restart attempts exposed that launchd’s sanitized
PATH resolves `python3` to macOS 3.9.6. The recovery helper failed on
`datetime.UTC`, while the policy helper evaluated modern union annotations at
import time. Both modules now run on the system interpreter:

- `/usr/bin/python3 -m py_compile` passes for `ccr_recovery`,
  `ccr_fallback_policy`, `ccr_wait_idle`, and `ccr_codex_catalog`.
- Focused imports/stamps pass under `/usr/bin/python3`.
- `python3 scripts/quality.py` passes all 12 checks.

## Non-claims

- The live completion proves one main-route chat request, not every provider,
  tool call, or streaming path.
- The process-vnode check is macOS-specific because it relies on `lsof` text
  mappings.
- Snapshot integrity proves recoverability of captured bytes, not semantic
  correctness of the state when it was captured.
