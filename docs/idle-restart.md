# Idle restarts

## Status

`scripts/ccr_wait_idle.py` reads Prometheus text output and only succeeds after
both of these are zero on two consecutive samples:

- `ccr_active_requests`
- `ccr_active_streams`

Two samples avoid restarting on a transient zero between the end of one model
round trip and the start of the next. The gate is probabilistic at a process
boundary; it reduces interruption risk, but cannot provide zero-downtime
connection handoff by itself.

```bash
python3 scripts/ccr_wait_idle.py \
  --url http://127.0.0.1:3456/metrics \
  --timeout 3600 \
  --json
```

The installed workstation wrappers use the gate before their default `restart`
verb. They retain `restart-now` for an explicit emergency override. Set
`CCR_IDLE_RESTART_TIMEOUT` to bound waiting; a busy listener exits 75 without
being restarted.

Build and install alone do not require restarting a listener. The running
process may continue using its old executable until an idle restart. Do not use
`launchctl kickstart -k` for routine changes: it can bypass CCR's graceful
SIGTERM drain and turn an in-flight LLM chain into an interrupted stream.
