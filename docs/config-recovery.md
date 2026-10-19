# Configuration recovery

CCR configuration is derived, not hand-maintained. One source policy
(`scripts/ccr_fallback_policy.py`) generates four consumer configs; several
launchd jobs own listeners; credential files inject real keys at start; and
Codex resolves each model slug against a local catalog at startup. That surface
is easy to damage and was previously recoverable only from ad hoc dated copies
scattered beside the live files, one of which was empty.

`ccr-recovery` makes the whole surface snapshot-able and restorable, so a bad
change is a rollback instead of an outage.

## Commands

| Command | Purpose |
| --- | --- |
| `ccr-recovery snapshot --label <name>` | Capture the live surface now |
| `ccr-recovery list` | List snapshots, newest first, with a verification result |
| `ccr-recovery verify [name]` | Re-hash one snapshot (default: newest) |
| `ccr-recovery restore [name]` | Dry run: report drift without writing |
| `ccr-recovery restore [name] --apply` | Write the snapshot back onto the live surface |
| `ccr-recovery restore [name] --apply --restart` | Also idle-gate and restart listeners |
| `ccr-recovery activate --listener <name>` | Idle-gate and restart one listener now |
| `ccr-recovery doctor` | Snapshot health, file drift, and live in-memory route drift |
| `ccr-recovery sanitize [name]` | Build a verified, secret-free copy for replication |

Every command prints JSON on stdout, so scripts and agents can consume it.
Exit code `0` means the requested operation succeeded and verified.

## What is captured

| Area | Files |
| --- | --- |
| `config-root/` | the four consumer configs, their launchd entrypoint scripts, `fallback-policy.json`, `runtime-credentials.json`, `ccr-api-key.env`, the deployed policy module, `.env`, `gmi_idle_activation.py` |
| `launch-agents/` | the plist for each CCR listener, the API tunnel, and the modelctl guard |
| `local-bin/` | `ccr-local`, `ccr-wait-idle`, `ccr-fallback-policy`, `ccr-catalog-sync`, `ccr-glm-workers-local`, `codex-ccr`. The retired `claude-ccr` wrapper is not captured, and a snapshot that still contains it reports it as unmapped instead of restoring it |
| `local-bin-targets/` | the files behind `local-bin` symlinks, so a link and its content are both recoverable |
| `toolbelt-manifests/`, `lapis-tools/` | both registered copies of the recovery, catalog, and idle-gate tool manifests |
| `share/`, `cloudflared/`, `syncthing/` | the deployed policy module, each tunnel `config.yml`, and the Syncthing config that owns the sanitized replication share |
| `codex/` | `ccr-live-model-catalog.json`, the active `config.toml`, CCR profile files, and the GLM worker role |

Snapshots live under `~/.local/share/ccr-recovery/snapshots`, created mode
`0700`, and each captured file keeps its own permission bits, because some live
files carry raw API keys.

## Automatic snapshots

`ccr-fallback-policy sync`, `serve.py`, and `ccr-catalog-sync` call
`ccr_recovery.pre_snapshot` before any write, so an ordinary routing or
metadata change captures the prior state with no operator step. A failed
snapshot never blocks the writer; the failure is reported on stderr, and the
writer's own atomic write remains the primary safety boundary. Both `--check`
modes are documented read-only and take no snapshot.

## Restoring

`restore` is a dry run unless `--apply` is passed. Apply stages every file
beside its destination, fsyncs, then moves it into place, so an interrupted
restore cannot leave a half-written config. Only after the files are written are
the consumer configs re-parsed; a restore that leaves invalid JSON does not
restart anything. With `--restart`, each listener waits on `ccr-wait-idle`
before `launchctl kickstart -k`, so a rollback never interrupts a live turn.

A symlink is restored as a symlink. The manifest records whether a captured
path was a link, because re-deriving that from live state cannot work: the whole
point of a restore is that live state has drifted, possibly replacing a link
with a regular file.

`doctor` also asks every governed listener for its in-memory default and tier
order through `/health?details=1`, then compares that with the corresponding
consumer config. The base `/health` endpoint remains content-free for ordinary
liveness probes. A listener can therefore be healthy but fail `doctor` after a
policy sync until it has drained and restarted; an older listener that answers
only the plain liveness contract is reported as runtime-unchecked rather than
mistaken for drift-free. Restore with `--restart` gates and restarts main,
worker, auth, and OCR.

## Policy integrity pin

`load_policy` gates the live `fallback-policy.json` on the approved route chain,
consumer registrations, and provider definitions. A whole-body SHA-256 pin
(`MACHINE_POLICY_INTEGRITY_SHA256`) closes the remaining gap: an agent editing
the JSON directly could change a field the structural checks do not constrain
while every checked list still matched. Changing the policy body requires a
source change with tests and review, not a JSON edit.

## Replication

`ccr-recovery sanitize` writes a verified copy of one snapshot for an external
share. It **omits** `.env`, `ccr-api-key.env`, `runtime-credentials.json`, and
Syncthing `config.xml` entirely rather than redacting or renaming them, then
fails closed if the remaining text files contain common literal
credential-assignment shapes or private-key headers. A placeholder or a renamed
copy in a shared folder is one mistake away from a live credential leak.
Restoring credentials stays a local operation against the private snapshot root.

## Non-claims

- A snapshot restores the state it captured; it does not prove that state was
  correct when captured.
- Restore does not contact providers. It proves files were written back and
  parse; it does not prove a credential still authenticates.
- `doctor` compares the newest snapshot against live files. It does not detect
  a change that was made and then reverted.
- The replication scanner detects common credential assignments and private-key
  headers; it is a fail-closed guard, not proof that every possible secret
  encoding has been classified.
- The idle gate observes request and stream gauges, so a listener that is busy
  but not serving through those gauges can still be interrupted by `--restart`.
