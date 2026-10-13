# Idle-restart gate, 2026-10-13

## Claim

CCR listener operators can require two consecutive Prometheus samples with both
`ccr_active_requests` and `ccr_active_streams` at zero before restarting. The
local main and worker wrapper `restart` verbs use this gate; `restart-now`
remains an explicit emergency override.

Implementation commits: `1ed5443` and review-repair commit `3e78705` on
`feat/idle-restart-gate`, stacked on `feat/quality-matrix` at `fd22748`.

## Source validation

All commands ran in
`/Users/kearm/ccr-rust/.worktrees/pr-idle-restart`.

- `python3 -m unittest discover -s scripts -p 'test_ccr_wait_idle.py' -v` —
  exit 0, 3 tests passed.
- `ruff check` and `ruff format --check` on the gate and tests — exit 0.
- `python3 scripts/quality.py` — exit 0. All six offline matrix checks passed.
- `cmp scripts/ccr_wait_idle.py ~/.local/bin/ccr-wait-idle` — exact match.
- Review-repair tree: `python3 -m unittest discover -s scripts -p
  'test_ccr_wait_idle.py' -v` — exit 0, 7 tests passed; `ruff check` and
  `ruff format --check` — exit 0; `python3 scripts/quality.py` — exit 0.

## Live validation

The exact source gate was invoked against the busy main listener:

```bash
python3 scripts/ccr_wait_idle.py \
  --url http://127.0.0.1:3456/metrics \
  --timeout 0.2 \
  --poll-interval 0.05 \
  --json
```

It returned exit 75 with `listener stayed busy or metrics did not become stable
before timeout`. The main listener PID remained `13529`.

The review-repaired source was also probed directly against the same busy
listener and again returned exit 75 without changing PID `13529`.

## Review repairs and retained boundaries

The repaired gate:

- clears a recovered transport error so a later busy timeout is not
  misreported as an old metrics failure;
- treats `http.client.HTTPException` as transient;
- accepts Prometheus labels, timestamps, and strict numeric values;
- rejects malformed numeric samples; and
- rejects `--stable-samples 1` as argparse usage exit 2 rather than temporary
  failure exit 75.

An SSL-verification bypass was not added: the governed local metrics endpoints
are HTTP and deliberately use normal certificate verification elsewhere. Exact
zero comparison remains intentional because any nonzero gauge, however small,
means the listener is not provably idle. Signal handling and changing exit 75
were not needed for the launchd wrapper contract.

The currently installed `~/.local/bin/ccr-wait-idle` remains the earlier
reviewed deployment until this PR is merged; direct source invocation above
validated the repaired PR head without changing live wrappers or restarting a
listener.

The installed wrappers are:

- `~/.local/bin/ccr-local`
- `~/.local/bin/ccr-glm-workers-local`

Their default `restart` verbs call `wait_idle`; their `restart-now` verbs retain
the emergency bypass. Before modification, both wrappers were backed up under
`~/.claude-code-router/backups/20261013-idle-restart/`.

## Non-claims

- This is not a zero-downtime socket handoff; a process-boundary race remains
  possible even after two idle samples.
- The worker listener was verified idle during the broader deployment work but
  was not restarted for this validation.
- No main, worker, OCR, or authenticated listener was restarted.
- The gate is Python operator tooling and does not change the Rust router
  binary's request-handling behavior.
