#!/usr/bin/env python3
"""Snapshot, restore, and verify the live CCR routing surface.

A change to CCR configuration is a normal operation: the shared policy module
derives four consumer configs, several launchd jobs own listeners, and
credential files inject real keys at start. That surface is easy to damage and
was previously recoverable only from ad hoc dated copies scattered beside the
live files, one of which was empty. This tool makes the whole surface
snapshot-able and restorable, so a bad change is a rollback instead of an
outage.

Design rules that the rest of this file depends on:

* Snapshots live under one private root (mode 0700) and every captured file
  keeps its own permission bits, because some live files carry raw API keys.
* A snapshot is only valid if it has a manifest and every file hash in that
  manifest verifies. ``verify`` never trusts a directory that merely exists.
* Restore is staged: files are written and fsynced beside their destination,
  then moved into place, so a failed restore cannot leave a half-written
  config. Consumers are restored, then validated, and only then are listeners
  restarted, each behind the idle gate.
* Nothing here contacts a provider or needs the router binary to be running.
  Snapshot and restore are filesystem operations; only the optional restart
  step talks to launchd.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path, PurePosixPath
from typing import Any

CONFIG_ROOT = Path.home() / ".claude-code-router"
CODEX_HOME = Path.home() / ".codex"
LAUNCH_AGENTS = Path.home() / "Library" / "LaunchAgents"
LOCAL_BIN = Path.home() / ".local" / "bin"
CLOUDFLARED = Path.home() / ".cloudflared"
SHARE_ROOT = Path.home() / ".local" / "share" / "ccr-routing"
TOOLBELT_MANIFESTS = Path.home() / "toolbelt" / "manifests"
LAPIS_TOOL_MANIFESTS = Path.home() / ".config" / "lapis" / "tools.d"
SYNCTHING_CONFIG = (
    Path.home() / "Library" / "Application Support" / "Syncthing" / "config.xml"
)

# Where snapshots are written. Deliberately outside CONFIG_ROOT so a snapshot
# can never restore or truncate itself, and outside /tmp because APFS keeps
# root-backed /tmp short-lived and the policy is to keep caches off it.
SNAPSHOT_ROOT = Path.home() / ".local" / "share" / "ccr-recovery" / "snapshots"

MANIFEST_NAME = "manifest.json"
MANIFEST_VERSION = 1

# Files under CONFIG_ROOT that are part of the live routing surface. Anything
# secret is captured because losing it means no provider can authenticate; the
# snapshot root is private, so capture is safe.
CONFIG_FILES: tuple[str, ...] = (
    "config.json",
    "config-auth.json",
    "glm-workers.json",
    "ocr-reviewers.json",
    "fallback-policy.json",
    "runtime-credentials.json",
    "ccr-api-key.env",
    "ccr_fallback_policy.py",
    "gmi_idle_activation.py",
    ".env",
)
LAUNCHER_FILES: tuple[str, ...] = (
    "serve.py",
    "serve-glm-workers.py",
    "shape_probe_auth.py",
)
TOOL_MANIFESTS: tuple[str, ...] = (
    "ccr-recovery.tool.json",
    "ccr-catalog-sync.tool.json",
    "ccr-wait-idle.tool.json",
)

# Client-side state that decides how a model behaves rather than where a request
# is routed. The catalog is load-bearing: Codex resolves a model slug against it
# at startup, and an unlisted slug silently falls back to generic metadata with
# no tool or context-window tuning. Losing it degrades every session instead of
# failing loudly, so it belongs in the same recovery surface as the configs.
CODEX_FILES: tuple[str, ...] = (
    "ccr-live-model-catalog.json",
    "config.toml",
    "ccr.config.toml",
    "claude-code-router.config.toml",
    "ccr-roles/glm-worker.toml",
)

# launchd jobs that own a CCR listener or its tunnel.
LAUNCH_AGENT_LABELS: tuple[str, ...] = (
    "com.kearm.ccr-rust",
    "com.kearm.ccr-rust-auth",
    "com.kearm.ccr-glm-workers",
    "com.kearm.ccr-ocr",
    "com.kearm.ccr-api-tunnel",
    "com.kearm.ccr-cordon-activate",
    "com.kearm.modelctl-guard-minimax",
)

# Operator wrappers and the deployed policy copy under version control.
#
# ``claude-ccr`` was retired on 2026-10-19: Claude Code on the maintainer's Mac
# is a native Anthropic subscription client and must not be routed through CCR.
# The wrapper is deliberately NOT in this inventory. Leaving it in would let
# ``ccr-recovery restore`` write a pre-retirement launcher back onto the
# machine, silently undoing the retirement; snapshots that still contain the
# file now report it as unmapped instead of restoring it.
BIN_FILES: tuple[str, ...] = (
    "ccr-local",
    "ccr-wait-idle",
    "ccr-fallback-policy",
    "ccr-catalog-sync",
    "ccr-glm-workers-local",
    "codex-ccr",
    "ccr-recovery",
)

# Deployed policy and recovery implementations. The local-bin copies are
# captured separately so restoring one implementation does not depend on the
# other still being current.
SHARE_FILES: tuple[str, ...] = (
    "ccr_fallback_policy.py",
    "ccr_recovery.py",
)

# Retired wrappers that older snapshots may still contain. They are reported as
# unmapped rather than restored, so a restore cannot resurrect them.
RETIRED_BIN_FILES: tuple[str, ...] = ("claude-ccr",)

# Live listeners and their launchd labels, used by the restart step.
LISTENERS: tuple[tuple[str, int, str, str], ...] = (
    ("main", 3456, "com.kearm.ccr-rust", "config.json"),
    ("worker", 3457, "com.kearm.ccr-glm-workers", "glm-workers.json"),
    ("auth", 3461, "com.kearm.ccr-rust-auth", "config-auth.json"),
    ("ocr", 3458, "com.kearm.ccr-ocr", "ocr-reviewers.json"),
)
LISTENER_BY_NAME = {
    name: (port, label, config_name) for name, port, label, config_name in LISTENERS
}

# How many snapshots to keep. Every snapshot is small (a few hundred KB), and a
# rollback story that prunes the good state to save space is worse than the
# space cost.
KEEP_SNAPSHOTS = 30
CCR_BINARY = Path.home() / ".cargo" / "bin" / "ccr-rust"

UMASK = os.umask(0)
os.umask(UMASK)


def now() -> dt.datetime:
    return dt.datetime.now(dt.UTC).astimezone()


def stamp(moment: dt.datetime | None = None) -> str:
    """Filesystem-safe timestamp that sorts chronologically and names the zone."""
    moment = moment or now()
    return moment.strftime("%Y%m%dT%H%M%S%z")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def file_mode(path: Path) -> int:
    return stat.S_IMODE(path.stat().st_mode)


def relative_targets() -> tuple[tuple[Path, str], ...]:
    """Every (live path, snapshot-relative name) pair in the live surface.

    Names are flattened to ``<area>/<basename>`` so two areas can hold files
    with the same basename without colliding in one snapshot directory.
    """
    targets: list[tuple[Path, str]] = []
    for name in CONFIG_FILES:
        targets.append((CONFIG_ROOT / name, f"config-root/{name}"))
    for name in LAUNCHER_FILES:
        targets.append((CONFIG_ROOT / name, f"config-root/{name}"))
    for name in CODEX_FILES:
        targets.append((CODEX_HOME / name, f"codex/{name}"))
    for label in LAUNCH_AGENT_LABELS:
        targets.append(
            (LAUNCH_AGENTS / f"{label}.plist", f"launch-agents/{label}.plist")
        )
    for name in BIN_FILES:
        targets.append((LOCAL_BIN / name, f"local-bin/{name}"))
    for name in SHARE_FILES:
        targets.append((SHARE_ROOT / name, f"share/{name}"))
    targets.append((SYNCTHING_CONFIG, "syncthing/config.xml"))
    for name in TOOL_MANIFESTS:
        targets.append((TOOLBELT_MANIFESTS / name, f"toolbelt-manifests/{name}"))
        targets.append((LAPIS_TOOL_MANIFESTS / name, f"lapis-tools/{name}"))
    for tunnel in sorted(CLOUDFLARED.glob("*/config.yml")):
        targets.append((tunnel, f"cloudflared/{tunnel.parent.name}/config.yml"))
    return tuple(targets)


def resolve_local(link: Path) -> Path | None:
    """Follow a local-bin symlink into its deployed target, keeping both sides.

    ``ccr-fallback-policy`` is a symlink to the deployed policy copy. Snapshotting
    the link target alone would lose the link itself; snapshotting the link alone
    would lose content the restore cannot verify. Capture the link and its
    resolved target under distinct names.
    """
    if not link.is_symlink():
        return None
    return Path(os.path.realpath(link))


def resolve_targets() -> tuple[tuple[Path, str], ...]:
    """Expand the live surface, adding the target of any local-bin symlink."""
    expanded: list[tuple[Path, str]] = []
    for live, name in relative_targets():
        expanded.append((live, name))
        if live.parent == LOCAL_BIN:
            target = resolve_local(live)
            if target is not None:
                expanded.append((target, f"local-bin-targets/{live.name}"))
    return tuple(expanded)


def safe_snapshot_name(name: object) -> bool:
    """Whether a manifest name is a supported path confined to one snapshot area."""
    if not isinstance(name, str) or not name or "\\" in name or "\x00" in name:
        return False
    path = PurePosixPath(name)
    if path.is_absolute() or any(part in ("", ".", "..") for part in path.parts):
        return False
    area, *rest = path.parts
    if area == "config-root":
        return len(rest) == 1
    if area == "codex":
        return len(rest) in (1, 2) and (len(rest) == 1 or rest[0] == "ccr-roles")
    if area in (
        "launch-agents",
        "local-bin",
        "local-bin-targets",
        "toolbelt-manifests",
        "lapis-tools",
    ):
        return len(rest) == 1
    if area == "cloudflared":
        return len(rest) == 2 and rest[1] == "config.yml"
    if area == "share":
        return len(rest) == 1 and rest[0] in SHARE_FILES
    if area == "syncthing":
        return name == "syncthing/config.xml"
    return False


def safe_symlink_target(target: object) -> bool:
    if not isinstance(target, str) or "\x00" in target or "\n" in target:
        return False
    path = Path(target)
    if path.is_absolute():
        try:
            return path.resolve().is_relative_to(Path.home().resolve())
        except (OSError, RuntimeError, ValueError):
            return False
    return ".." not in path.parts


def ensure_private_directory(path: Path) -> None:
    """Create a directory tree that only the owner can enter.

    Snapshots may contain raw API keys, so the snapshot root and every snapshot
    directory are created 0700 rather than inheriting the umask.
    """
    path.mkdir(parents=True, exist_ok=True)
    try:
        os.chmod(path, 0o700)
    except OSError:
        # A filesystem without POSIX modes (or a non-POSIX mount) cannot hold
        # the guarantee; the caller still gets a working directory.
        pass


def prune(root: Path, keep: int = KEEP_SNAPSHOTS) -> list[str]:
    """Delete the oldest snapshots beyond ``keep``. Returns removed names.

    A directory is only a candidate when it has a manifest; a half-written
    snapshot from an interrupted run is reported by ``verify`` and pruned only
    once it is older than the retention window, so a crash cannot silently
    consume the newest good state.
    """
    snapshots = sorted(
        entry
        for entry in root.iterdir()
        if entry.is_dir() and (entry / MANIFEST_NAME).is_file()
    )
    removed: list[str] = []
    for entry in snapshots[: max(0, len(snapshots) - keep)]:
        shutil.rmtree(entry, ignore_errors=True)
        removed.append(entry.name)
    return removed


def write_manifest(
    snapshot: Path, label: str, symlinks: frozenset[str] = frozenset()
) -> dict[str, Any]:
    """Hash every captured file and write the snapshot manifest.

    ``mode`` is stored alongside each file so restore can reproduce the live
    permission bits; the mode is part of the security posture here, not an
    incidental detail.
    """
    files: dict[str, dict[str, Any]] = {}
    for captured in sorted(p for p in snapshot.rglob("*") if p.is_file()):
        relative = captured.relative_to(snapshot).as_posix()
        if relative == MANIFEST_NAME:
            continue
        record: dict[str, Any] = {
            "sha256": sha256_file(captured),
            "mode": file_mode(captured),
            "size": captured.stat().st_size,
        }
        # Whether a path was a symlink is a property of the captured state, so
        # it is recorded here. Re-deriving it from live state at restore time
        # cannot work: the whole point of a restore is that live state may have
        # drifted, possibly replacing a link with a regular file.
        if relative in symlinks:
            record["symlink"] = True
        files[relative] = record
    manifest = {
        "manifest_version": MANIFEST_VERSION,
        "label": label,
        "created_at": now().isoformat(timespec="seconds"),
        "platform": os.uname().sysname if hasattr(os, "uname") else sys.platform,
        "snapshot": snapshot.name,
        "files": files,
    }
    manifest_path = snapshot / MANIFEST_NAME
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    os.chmod(manifest_path, 0o600)
    return manifest


def snapshot(label: str = "manual", root: Path | None = None) -> dict[str, Any]:
    """Capture the live surface into a new timestamped snapshot directory.

    The snapshot is staged in a temporary sibling and moved into place only
    after the manifest is written, so an interrupted run cannot leave a
    directory that looks like a valid snapshot.
    """
    root = root or SNAPSHOT_ROOT
    ensure_private_directory(root)
    moment = now()
    final = root / stamp(moment)
    suffix = 0
    while final.exists():
        suffix += 1
        final = root / f"{stamp(moment)}-{suffix:02d}"
    staging = Path(tempfile.mkdtemp(prefix=f".{stamp(moment)}.", dir=root))
    captured: list[str] = []
    missing: list[str] = []
    symlinks: set[str] = set()
    try:
        for live, name in resolve_targets():
            if not live.exists() and not live.is_symlink():
                missing.append(name)
                continue
            destination = staging / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            if live.is_symlink():
                destination.write_text(os.readlink(live) + "\n")
                os.chmod(destination, 0o644)
            else:
                shutil.copy2(live, destination)
            if live.is_symlink():
                symlinks.add(name)
            captured.append(name)
        manifest = write_manifest(staging, label, frozenset(symlinks))
        os.replace(staging, final)
    except Exception:
        shutil.rmtree(staging, ignore_errors=True)
        raise
    removed = prune(root)
    return {
        "snapshot": final.name,
        "path": str(final),
        "label": label,
        "created_at": manifest["created_at"],
        "captured": captured,
        "missing": missing,
        "pruned": removed,
        "verified": verify_snapshot(final),
    }


def read_manifest(snapshot: Path) -> dict[str, Any]:
    manifest = json.loads((snapshot / MANIFEST_NAME).read_text(encoding="utf-8"))
    if manifest.get("manifest_version") != MANIFEST_VERSION:
        raise ValueError(f"Unsupported manifest version: {snapshot.name}")
    files = manifest.get("files")
    if not isinstance(files, dict):
        raise TypeError(f"Malformed file map: {snapshot.name}")
    for name, record in files.items():
        if not safe_snapshot_name(name):
            raise ValueError(f"Unsafe snapshot path: {name!r}")
        if not isinstance(record, dict):
            raise TypeError(f"Malformed snapshot record: {name}")
        digest = record.get("sha256")
        mode = record.get("mode")
        size = record.get("size")
        if (
            not isinstance(digest, str)
            or re.fullmatch(r"[0-9a-f]{64}", digest) is None
            or isinstance(mode, bool)
            or not isinstance(mode, int)
            or not 0 <= mode <= 0o7777
            or isinstance(size, bool)
            or not isinstance(size, int)
            or size < 0
        ):
            raise ValueError(f"Invalid snapshot file metadata: {name}")
    return manifest


def verify_snapshot(snapshot: Path) -> dict[str, Any]:
    """Check a snapshot exists, is complete, and every file hash matches.

    This is the trust boundary for restore: a directory without a readable
    manifest is not a snapshot, and a manifest whose files do not hash is not a
    recoverable state.
    """
    manifest_path = snapshot / MANIFEST_NAME
    if not manifest_path.is_file():
        return {"snapshot": snapshot.name, "ok": False, "error": "no manifest"}
    try:
        manifest = read_manifest(snapshot)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        return {"snapshot": snapshot.name, "ok": False, "error": str(error)}
    mismatched: list[str] = []
    missing: list[str] = []
    for relative, record in manifest["files"].items():
        captured = snapshot / relative
        if not captured.is_file():
            missing.append(relative)
            continue
        if record.get("symlink"):
            try:
                target = captured.read_text(encoding="utf-8").strip()
            except (OSError, UnicodeDecodeError):
                target = None
            if not safe_symlink_target(target):
                mismatched.append(relative)
                continue
        if sha256_file(captured) != record["sha256"]:
            mismatched.append(relative)
    extra = sorted(
        captured.relative_to(snapshot).as_posix()
        for captured in snapshot.rglob("*")
        if captured.is_file()
        and captured.name != MANIFEST_NAME
        and captured.relative_to(snapshot).as_posix() not in manifest["files"]
    )
    return {
        "snapshot": snapshot.name,
        "ok": not (mismatched or missing),
        "label": manifest.get("label"),
        "created_at": manifest.get("created_at"),
        "file_count": len(manifest["files"]),
        "missing": missing,
        "mismatched": mismatched,
        "unexpected": extra,
    }


def stage_and_replace(
    destination: Path, source: Path, mode: int, symlink_target: str | None
) -> None:
    """Write one file to its destination atomically, preserving mode or link.

    ``atomic_json`` in the policy module solves this for JSON configs. Restore
    needs the same guarantee for arbitrary files: stage beside the destination,
    fsync, then ``os.replace``, so a crash mid-restore never leaves a
    half-written config. Symlinks are recreated as symlinks, never as a copy of
    their contents, so ``ccr-fallback-policy`` stays a link.
    """
    destination.parent.mkdir(parents=True, exist_ok=True)
    if symlink_target is not None:
        link_target = symlink_target
        temporary = destination.with_name(f".{destination.name}.{os.getpid()}.link")
        if temporary.is_symlink() or temporary.exists():
            temporary.unlink()
        os.symlink(link_target, temporary)
        os.replace(temporary, destination)
        return
    fd, temporary_name = tempfile.mkstemp(
        prefix=f".{destination.name}.", dir=destination.parent
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(fd, "wb") as output:
            os.fchmod(output.fileno(), mode)
            with source.open("rb") as handle:
                shutil.copyfileobj(handle, output)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)


def restore_target(snapshot: Path, record: dict[str, Any]) -> dict[str, Any]:
    """Restore one manifest entry to its live destination."""
    source = snapshot / record["relative"]
    if not source.is_file():
        return {"relative": record["relative"], "ok": False, "error": "absent"}
    live = Path(record["live"])
    if record.get("symlink"):
        target = source.read_text(encoding="utf-8").strip()
        stage_and_replace(live, source, 0o777, target)
        return {"relative": record["relative"], "ok": True, "action": "symlink"}
    stage_and_replace(live, source, int(record["mode"]), None)
    return {"relative": record["relative"], "ok": True, "action": "file"}


def plan_restore(snapshot: Path) -> dict[str, Any]:
    """Map every manifest entry back to its live destination.

    The manifest stores only snapshot-relative paths, so the live destination
    is recomputed here from the same surface definition used by snapshot. That
    keeps one authority for "where does this file live" instead of trusting a
    path recorded on disk by whichever run took the snapshot.

    A captured symlink is stored as its target *path* in an ordinary file, so
    the manifest's ``symlink`` flag, not the live filesystem, decides how it is
    written back. Re-deriving that from live state cannot work: the entire
    point of a restore is that live state has drifted, possibly replacing a
    link with a regular file.
    """
    manifest = read_manifest(snapshot)
    captured = manifest["files"]
    live_by_name = {name: live for live, name in relative_targets()}
    entries: list[dict[str, Any]] = []
    unmapped: list[str] = []
    for name, record in sorted(captured.items()):
        symlink = False
        retired = (
            name.startswith(("local-bin/", "local-bin-targets/"))
        ) and name.split("/", 1)[1] in RETIRED_BIN_FILES
        if retired:
            # A retired wrapper captured by an older snapshot, as a regular file
            # or as the target behind a symlink. Report it instead of restoring
            # it, so a restore cannot undo the retirement.
            unmapped.append(name)
            continue
        if name in live_by_name:
            live = live_by_name[name]
            # The link itself: recreate the link pointing at the stored path.
            symlink = bool(record.get("symlink"))
        elif name.startswith("local-bin-targets/"):
            # The snapshot's link record is authoritative even if the live
            # link was removed or redirected during the drift being repaired.
            link_name = name.split("/", 1)[1]
            link_snapshot = snapshot / "local-bin" / link_name
            try:
                target = link_snapshot.read_text(encoding="utf-8").strip()
            except (OSError, UnicodeDecodeError):
                target = ""
            if not safe_symlink_target(target):
                unmapped.append(name)
                continue
            live = Path(target)
        else:
            if not name.startswith("cloudflared/"):
                unmapped.append(name)
                continue
            live = CLOUDFLARED / name.split("/", 1)[1]
        entries.append(
            {
                "relative": name,
                "live": str(live),
                "mode": int(record["mode"]),
                "sha256": record["sha256"],
                "symlink": symlink,
            }
        )
    return {
        "snapshot": snapshot.name,
        "label": manifest.get("label"),
        "created_at": manifest.get("created_at"),
        "entries": entries,
        "unmapped": unmapped,
    }


def validate_configs(
    configs: tuple[str, ...] = (
        "config.json",
        "config-auth.json",
        "glm-workers.json",
        "ocr-reviewers.json",
    ),
) -> dict[str, Any]:
    """Syntax-check every restored consumer config before anything restarts.

    A config that parses is the minimum bar; provider credentials are not
    probed here because restore must not depend on network reachability.
    """
    results: list[dict[str, Any]] = []
    for name in configs:
        path = CONFIG_ROOT / name
        if not path.is_file():
            results.append({"file": name, "ok": False, "error": "absent"})
            continue
        try:
            json.loads(path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as error:
            results.append({"file": name, "ok": False, "error": str(error)})
        else:
            results.append({"file": name, "ok": True})
    return {"ok": all(item["ok"] for item in results), "configs": results}


def wait_idle(port: int, timeout: int) -> dict[str, Any]:
    """Use the repo idle gate so a rollback never interrupts an active turn."""
    binary = LOCAL_BIN / "ccr-wait-idle"
    if not binary.is_file():
        return {"port": port, "ok": True, "skipped": "ccr-wait-idle absent"}
    try:
        completed = subprocess.run(
            [
                str(binary),
                "--url",
                f"http://127.0.0.1:{port}/metrics",
                "--timeout",
                str(timeout),
                "--json",
            ],
            capture_output=True,
            text=True,
            check=False,
            timeout=timeout + 30,
        )
    except subprocess.TimeoutExpired:
        return {"port": port, "ok": False, "error": "idle gate timed out"}
    return {
        "port": port,
        "ok": completed.returncode == 0,
        "exit": completed.returncode,
        "output": (completed.stdout or completed.stderr).strip()[:400],
    }


def listener_pid(port: int) -> int | None:
    """Return the PID currently owning a listener socket."""
    try:
        completed = subprocess.run(
            ["lsof", "-tnP", f"-iTCP:{port}", "-sTCP:LISTEN"],
            capture_output=True,
            text=True,
            check=False,
            timeout=5,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    pids = [int(line) for line in completed.stdout.splitlines() if line.isdigit()]
    return pids[0] if len(pids) == 1 else None


def process_text_mappings(pid: int) -> list[tuple[str, int]]:
    """Return an executable's mapped text paths and vnode inode numbers."""
    try:
        completed = subprocess.run(
            ["lsof", "-Ffin", "-p", str(pid)],
            capture_output=True,
            text=True,
            check=False,
            timeout=5,
        )
    except (OSError, subprocess.TimeoutExpired):
        return []
    mappings: list[tuple[str, int]] = []
    fd: str | None = None
    inode: int | None = None
    for line in completed.stdout.splitlines():
        marker, value = line[:1], line[1:]
        if marker == "f":
            fd, inode = value, None
        elif marker == "i" and value.isdigit():
            inode = int(value)
        elif marker == "n" and fd == "txt" and inode is not None:
            mappings.append((value, inode))
            fd, inode = None, None
    return mappings


def listener_image(port: int) -> dict[str, Any]:
    """Identify the process and prove it is executing the installed image."""
    pid = listener_pid(port)
    if pid is None:
        return {"ok": False, "error": "no unique listener PID"}
    result: dict[str, Any] = {"ok": False, "pid": pid}
    try:
        expected_stat = CCR_BINARY.stat()
    except OSError as error:
        result["error"] = f"installed binary unavailable: {error}"
        return result
    expected_path = os.path.realpath(CCR_BINARY)
    for mapped_path, inode in process_text_mappings(pid):
        if os.path.realpath(mapped_path) == expected_path:
            result.update(
                executable=mapped_path,
                vnode_inode=inode,
                installed_inode=expected_stat.st_ino,
            )
            result["ok"] = inode == expected_stat.st_ino
            if not result["ok"]:
                result["error"] = "listener still maps the replaced executable inode"
            return result
    result["error"] = f"listener does not map {expected_path}"
    return result


def restart_listener(
    label: str,
    port: int,
    timeout: int,
    idle_timeout: int,
    *,
    verify_image: bool = True,
) -> dict[str, Any]:
    """Idle-gate, restart, and prove the listener is healthy and current."""
    idle = wait_idle(port, idle_timeout)
    if not idle.get("ok"):
        return {"label": label, "port": port, "ok": False, "stage": "idle", **idle}
    before = listener_image(port) if verify_image else {}
    domain = f"gui/{os.getuid()}/{label}"
    try:
        completed = subprocess.run(
            ["launchctl", "kickstart", "-k", domain],
            capture_output=True,
            text=True,
            check=False,
            timeout=60,
        )
    except subprocess.TimeoutExpired:
        return {"label": label, "port": port, "ok": False, "stage": "kickstart"}
    if completed.returncode != 0:
        return {
            "label": label,
            "port": port,
            "ok": False,
            "stage": "kickstart",
            "error": completed.stderr.strip()[:200],
        }
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        image = listener_image(port) if verify_image else {"ok": True}
        if listener_healthy(port) and image.get("ok"):
            return {
                "label": label,
                "port": port,
                "ok": True,
                "stage": "restarted",
                "idle": idle,
                "before": before,
                "image": image,
            }
        time.sleep(0.25)
    return {
        "label": label,
        "port": port,
        "ok": False,
        "stage": "image" if verify_image else "health",
        "before": before,
    }


def listener_healthy(port: int) -> bool:
    import urllib.error
    import urllib.request

    try:
        with urllib.request.urlopen(
            f"http://127.0.0.1:{port}/health", timeout=2
        ) as response:
            response.read(16)
            return response.status == 200
    except (OSError, urllib.error.URLError):
        return False


def activate_listener(
    name: str,
    *,
    idle_timeout: int = 900,
    restart_timeout: int = 60,
    live_check: bool = False,
    root: Path | None = None,
) -> dict[str, Any]:
    try:
        port, label, _config_name = LISTENER_BY_NAME[name]
    except KeyError:
        return {
            "listener": name,
            "ok": False,
            "error": f"unknown listener: {name}",
            "available": sorted(LISTENER_BY_NAME),
        }
    preflight = snapshot(label=f"pre-activate-{name}", root=root)
    if not preflight.get("verified", {}).get("ok") or preflight.get("missing"):
        return {
            "listener": name,
            "label": label,
            "port": port,
            "ok": False,
            "stage": "snapshot",
            "snapshot": preflight,
        }
    restarted = restart_listener(label, port, restart_timeout, idle_timeout)
    result = {
        "listener": name,
        "label": label,
        "port": port,
        "snapshot": {
            key: preflight[key]
            for key in ("snapshot", "path", "created_at", "verified")
        },
        **restarted,
    }
    if not restarted.get("ok"):
        return result
    runtime = runtime_listener_check(name, port, _config_name)
    result["runtime"] = runtime
    if live_check:
        result["live_request"] = representative_listener_request(port)
        result["ok"] = bool(runtime.get("ok") and result["live_request"].get("ok"))
    else:
        result["ok"] = bool(runtime.get("ok"))
    return result


def representative_listener_request(port: int, timeout: int = 60) -> dict[str, Any]:
    """Make a tiny provider-backed request after an explicit operator opt-in."""
    import urllib.error
    import urllib.request

    runtime = listener_runtime(port)
    model = runtime.get("default")
    if not isinstance(model, str) or not model:
        return {"ok": False, "error": "listener has no default model"}
    body = json.dumps(
        {
            "model": model,
            "reasoning_effort": "low",
            "messages": [
                {"role": "user", "content": "Return exactly OK and no other text."}
            ],
            "max_tokens": 64,
            "stream": False,
        }
    ).encode()
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}/v1/chat/completions",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(  # noqa: S310 - fixed loopback URL
            request, timeout=timeout
        ) as response:
            document = json.loads(response.read().decode())
    except (
        OSError,
        urllib.error.URLError,
        json.JSONDecodeError,
        UnicodeDecodeError,
    ) as error:
        return {"ok": False, "error": f"{type(error).__name__}: {error}"}
    choices = document.get("choices") if isinstance(document, dict) else None
    choice = choices[0] if isinstance(choices, list) and choices else None
    if not isinstance(choice, dict):
        return {"ok": False, "error": "completion response has no valid choice"}
    message = choice.get("message")
    content = message.get("content") if isinstance(message, dict) else None
    ok = isinstance(content, str) and bool(content.strip())
    return {
        "ok": ok,
        "model": document.get("model") if isinstance(document, dict) else None,
        "finish_reason": choice.get("finish_reason"),
        "content": content,
        "usage": document.get("usage") if isinstance(document, dict) else None,
    }


def listener_runtime(port: int) -> dict[str, Any]:
    import urllib.error
    import urllib.request

    try:
        with urllib.request.urlopen(
            f"http://127.0.0.1:{port}/health?details=1", timeout=2
        ) as response:
            payload = response.read().decode()
    except (
        OSError,
        urllib.error.URLError,
        json.JSONDecodeError,
        UnicodeDecodeError,
    ) as error:
        return {"ok": False, "error": f"{type(error).__name__}: {error}"}
    try:
        document = json.loads(payload)
    except json.JSONDecodeError:
        return {
            "ok": False,
            "error": "listener uses the older content-free /health contract",
            "health": payload[:200],
        }
    if not isinstance(document, dict) or document.get("status") != "ok":
        return {
            "ok": False,
            "error": "health payload did not report ok",
            "health": document,
        }
    tiers = document.get("tiers")
    default = document.get("default")
    if not isinstance(tiers, list) or not all(isinstance(tier, str) for tier in tiers):
        return {
            "ok": False,
            "error": "health payload has invalid tiers",
            "health": document,
        }
    return {"ok": True, "tiers": tiers, "default": default}


def runtime_listener_check(name: str, port: int, config_name: str) -> dict[str, Any]:
    check: dict[str, Any] = {"name": name, "port": port, "config": config_name}
    try:
        configured = json.loads((CONFIG_ROOT / config_name).read_text())
        expected = configured["Router"]["tiers"]
    except (OSError, json.JSONDecodeError, KeyError, TypeError) as error:
        check.update(
            ok=False,
            error=f"cannot read configured tier order: {type(error).__name__}: {error}",
        )
        return check
    runtime = listener_runtime(port)
    check.update(runtime)
    check["expected_tiers"] = expected
    check["ok"] = bool(runtime.get("ok")) and runtime.get("tiers") == expected
    if runtime.get("ok") and runtime.get("tiers") != expected:
        check["error"] = "listener has a stale in-memory tier order"
    return check


def runtime_listener_checks() -> list[dict[str, Any]]:
    return [
        runtime_listener_check(name, port, config_name)
        for name, port, _label, config_name in LISTENERS
    ]


def restore(
    snapshot: Path,
    *,
    apply: bool = False,
    restart: bool = False,
    selected: tuple[str, ...] = (),
    idle_timeout: int = 900,
    restart_timeout: int = 60,
) -> dict[str, Any]:
    """Verify, then optionally apply, a snapshot back onto the live surface.

    Dry run is the default: it verifies hashes, resolves every destination and
    reports the diff without touching a single live file. ``apply=True`` is the
    only path that writes, and it is deliberately explicit.
    """
    verification = verify_snapshot(snapshot)
    if not verification.get("ok"):
        return {
            "snapshot": snapshot.name,
            "ok": False,
            "stage": "verify",
            "verification": verification,
        }
    plan = plan_restore(snapshot)
    entries = plan["entries"]
    if selected:
        entries = [
            entry
            for entry in entries
            if entry["live"] in selected or entry["relative"] in selected
        ]
    changes: list[dict[str, Any]] = []
    for entry in entries:
        live = Path(entry["live"])
        status: dict[str, Any] = {"relative": entry["relative"], "live": entry["live"]}
        if not live.exists() and not live.is_symlink():
            status["state"] = "missing"
        elif entry["symlink"] and live.is_symlink():
            expected_target = (
                (snapshot / entry["relative"]).read_text(encoding="utf-8").strip()
            )
            status["state"] = (
                "unchanged" if os.readlink(live) == expected_target else "symlink-drift"
            )
        elif not entry["symlink"] and live.is_symlink():
            status["state"] = "type-drift"
        elif live.is_file():
            status["state"] = (
                "unchanged" if sha256_file(live) == entry["sha256"] else "modified"
            )
            if file_mode(live) != entry["mode"]:
                status["mode_drift"] = {
                    "current": file_mode(live),
                    "snapshot": entry["mode"],
                }
        else:
            status["state"] = "type-drift"
        changes.append(status)
    result: dict[str, Any] = {
        "snapshot": plan["snapshot"],
        "label": plan["label"],
        "created_at": plan["created_at"],
        "applied": apply,
        "verification": verification,
        "changes": changes,
        "unmapped": plan["unmapped"],
        "drift": [c for c in changes if c.get("state") not in (None, "unchanged")],
    }
    if not apply:
        result["ok"] = True
        result["stage"] = "dry-run"
        return result
    restored: list[dict[str, Any]] = []
    for entry in entries:
        record = {
            "relative": entry["relative"],
            "live": entry["live"],
            "mode": entry["mode"],
            "symlink": entry["symlink"],
        }
        restored.append(restore_target(snapshot, record))
    result["restored"] = restored
    failed = [item for item in restored if not item["ok"]]
    # Validate what this restore actually touched. A scoped restore of one
    # wrapper must not be failed by a consumer config this run never wrote.
    touched_configs = tuple(
        entry["relative"].split("/", 1)[1]
        for entry in entries
        if entry["relative"].startswith("config-root/")
        and entry["relative"].split("/", 1)[1].endswith(".json")
    )
    validation = validate_configs(touched_configs)
    result["validation"] = validation
    result["ok"] = not failed and validation["ok"]
    result["stage"] = "applied"
    if failed or not validation["ok"]:
        result["restarted"] = []
        result["note"] = (
            "restore incomplete or configs invalid; listeners left untouched"
        )
        return result
    if restart:
        result["restarted"] = [
            restart_listener(label, port, restart_timeout, idle_timeout)
            for _, port, label, _config in LISTENERS
        ]
        result["ok"] = all(item["ok"] for item in result["restarted"])
    return result


def pre_snapshot(root: Path = SNAPSHOT_ROOT, label: str = "auto") -> dict[str, Any]:
    """Best-effort snapshot before a caller mutates the live surface.

    Used as a hook by config writers (``ccr-fallback-policy sync``, ``serve``).
    A failed snapshot must never block the caller, because a routing emergency
    is not the moment to discover that snapshot permissions drifted; the
    caller's own atomic write is the primary safety boundary, and the snapshot
    is the recovery layer. The failure is still reported on stderr and in the
    return value, so a broken recovery layer is visible rather than silent.
    """
    try:
        return snapshot(label=label, root=root)
    except OSError as error:
        message = {
            "ok": False,
            "error": f"{type(error).__name__}: {error}",
            "label": label,
        }
        print(
            f"ccr-recovery: pre-write snapshot failed: {message['error']}",
            file=sys.stderr,
        )
        return message


def doctor(root: Path = SNAPSHOT_ROOT, max_age_hours: float = 24.0) -> dict[str, Any]:
    """Report snapshot health: newest, whether it verifies, and drift vs live.

    This is the continuous check an operator (or a scheduled probe) runs to
    catch a silent gap before it matters, not after a failure.
    """
    ensure_private_directory(root)
    snapshots = sorted(
        entry
        for entry in root.iterdir()
        if entry.is_dir() and (entry / MANIFEST_NAME).is_file()
    )
    newest: dict[str, Any] | None = None
    if snapshots:
        newest = verify_snapshot(snapshots[-1])
    live_by_name = {name: live for live, name in resolve_targets()}
    captured: dict[str, str] = {}
    symlinked: set[str] = set()
    if newest and newest.get("ok"):
        manifest = read_manifest(snapshots[-1])
        captured = {
            relative: record["sha256"] for relative, record in manifest["files"].items()
        }
        symlinked = {
            relative
            for relative, record in manifest["files"].items()
            if record.get("symlink")
        }
    changed: list[str] = []
    for name, digest in sorted(captured.items()):
        live = live_by_name.get(name)
        if live is None:
            continue
        if name in symlinked:
            # Hashing a symlink follows it and hashes the target's content,
            # which never equals the stored link path. Compare the link itself.
            if not live.is_symlink() or (
                (snapshot_path := snapshots[-1] / name).is_file()
                and os.readlink(live) != snapshot_path.read_text().strip()
            ):
                changed.append(name)
            continue
        if not live.is_file():
            changed.append(name)
            continue
        if sha256_file(live) != digest:
            changed.append(name)
    missing_on_disk = sorted(
        name
        for name, live in live_by_name.items()
        if not live.exists() and not live.is_symlink()
    )
    age_hours: float | None = None
    if newest and newest.get("created_at"):
        created = dt.datetime.fromisoformat(newest["created_at"])
        if created.tzinfo is None:
            created = created.replace(tzinfo=dt.UTC)
        age_hours = (now() - created).total_seconds() / 3600.0
    runtime = runtime_listener_checks()
    runtime_ok = all(check["ok"] for check in runtime)
    return {
        "ok": bool(newest and newest.get("ok")) and runtime_ok,
        "snapshot_root": str(root),
        "snapshot_count": len(snapshots),
        "newest": newest,
        "age_hours": age_hours,
        "max_age_hours": max_age_hours,
        "drifted_vs_live": changed,
        "missing_from_live": missing_on_disk,
        "runtime_ok": runtime_ok,
        "runtime_listeners": runtime,
    }


def list_snapshots(root: Path = SNAPSHOT_ROOT) -> list[dict[str, Any]]:
    """List all snapshots, newest first, with a per-snapshot verification result."""
    ensure_private_directory(root)
    snapshots = sorted(
        (entry for entry in root.iterdir() if entry.is_dir()),
        key=lambda entry: entry.name,
        reverse=True,
    )
    return [verify_snapshot(entry) for entry in snapshots]


SANITIZED_ROOT = Path.home() / ".local" / "share" / "ccr-recovery" / "shared"
SANITIZED_MANIFEST = "sanitized-manifest.json"

# These files exist only to authenticate to providers and auth clients. The
# public recovery surface deliberately omits them entirely: a placeholder or a
# renamed copy in a shared folder is one programming mistake away from a live
# credential leak, as this tool's own review process proved before deployment.
SECRET_BASENAMES: frozenset[str] = frozenset(
    {
        ".env",
        "ccr-api-key.env",
        "runtime-credentials.json",
        "config.xml",
    }
)

SECRET_ASSIGNMENT = re.compile(
    r"\b(?:api[_-]?key|access[_-]?token|auth[_-]?token|token|secret|password)\b"
    r"[\"']?\s*[:=]\s*[\"']?(?P<value>[^\s\"',;}]+)",
    re.IGNORECASE,
)
PRIVATE_KEY_HEADER = re.compile(
    r"-----BEGIN (?:RSA |EC |OPENSSH |DSA |ENCRYPTED )?PRIVATE KEY-----"
)


def sanitize_snapshot(
    snapshot: Path,
    root: Path = SNAPSHOT_ROOT,
    destination_root: Path = SANITIZED_ROOT,
) -> dict[str, Any]:
    """Build a verified, secret-free copy for off-machine replication.

    Secret-bearing files are absent from the output, not redacted, renamed, or
    emptied in place. A share only ever sees routing shapes, policy source,
    launchd definitions, and wrappers; restoring credentials remains a local
    operation against the private snapshot root.
    """
    verification = verify_snapshot(snapshot)
    if not verification.get("ok"):
        return {
            "ok": False,
            "stage": "verify",
            "snapshot": snapshot.name,
            "verification": verification,
        }
    destination = destination_root / snapshot.name
    manifest = read_manifest(snapshot)
    copy_names = [
        name
        for name in sorted(manifest["files"])
        if Path(name).name not in SECRET_BASENAMES
    ]
    secret_errors = scan_snapshot_for_secrets(snapshot, tuple(copy_names))
    if secret_errors:
        return {
            "ok": False,
            "stage": "secret-scan",
            "snapshot": snapshot.name,
            "errors": secret_errors,
        }
    if destination.exists():
        shutil.rmtree(destination)
    ensure_private_directory(destination)
    copied: list[str] = []
    omitted: list[str] = []
    for name in sorted(manifest["files"]):
        source = snapshot / name
        if Path(name).name in SECRET_BASENAMES:
            omitted.append(name)
            continue
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        copied.append(name)
    output = {
        "sanitized_manifest_version": MANIFEST_VERSION,
        "source_snapshot": snapshot.name,
        "label": manifest.get("label"),
        "created_at": now().isoformat(timespec="seconds"),
        "secret_files_omitted": omitted,
        "files": {
            name: {
                "sha256": sha256_file(destination / name),
                "size": (destination / name).stat().st_size,
            }
            for name in copied
        },
    }
    output_path = destination / SANITIZED_MANIFEST
    output_path.write_text(json.dumps(output, indent=2, sort_keys=True) + "\n")
    os.chmod(output_path, 0o600)
    return {
        "ok": True,
        "snapshot": snapshot.name,
        "path": str(destination),
        "copied": len(copied),
        "omitted": omitted,
    }


def scan_snapshot_for_secrets(snapshot: Path, names: tuple[str, ...]) -> list[str]:
    """Fail closed on common literal credential shapes before exporting a share."""
    errors: list[str] = []
    for name in names:
        source = snapshot / name
        try:
            payload = source.read_bytes()
        except OSError as error:
            errors.append(f"{name}: cannot scan: {type(error).__name__}: {error}")
            continue
        if b"\x00" in payload:
            continue
        text = payload.decode("utf-8", errors="replace")
        if PRIVATE_KEY_HEADER.search(text):
            errors.append(f"{name}: private-key material found")
            continue
        for match in SECRET_ASSIGNMENT.finditer(text):
            value = match.group("value")
            placeholder = value.startswith("${") or re.fullmatch(
                r"\$[A-Za-z_][A-Za-z0-9_]*", value
            )
            programmatic_lookup = "(" in value
            if len(value) >= 16 and not placeholder and not programmatic_lookup:
                errors.append(f"{name}: literal credential-shaped value found")
                break
    return errors


def parse_args(argv: list[str] | None = None) -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="ccr-recovery",
        description="Snapshot, restore, and verify the live CCR routing surface.",
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=SNAPSHOT_ROOT,
        help="Snapshot root (default: %(default)s)",
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    snap = subparsers.add_parser("snapshot", help="Capture the live surface now.")
    snap.add_argument(
        "--label", default="manual", help="Operator label for this snapshot."
    )

    subparsers.add_parser("list", help="List snapshots, newest first.")

    verify = subparsers.add_parser(
        "verify", help="Verify one snapshot (default: newest)."
    )
    verify.add_argument("snapshot_name", nargs="?", help="Snapshot directory name.")

    restore_parser = subparsers.add_parser("restore", help="Restore from a snapshot.")
    restore_parser.add_argument(
        "snapshot_name", nargs="?", help="Snapshot directory name (default: newest)."
    )
    restore_parser.add_argument(
        "--apply", action="store_true", help="Actually write files (default: dry run)."
    )
    restore_parser.add_argument(
        "--restart",
        action="store_true",
        help="Idle-gate and restart listeners after restore.",
    )
    restore_parser.add_argument(
        "--select",
        action="append",
        default=[],
        metavar="PATH_OR_RELATIVE",
        help="Limit restore to a live path or a snapshot-relative name (repeatable).",
    )
    restore_parser.add_argument(
        "--idle-timeout",
        type=int,
        default=900,
        help="Seconds to wait for idle (default: 900).",
    )
    restore_parser.add_argument(
        "--restart-timeout",
        type=int,
        default=60,
        help="Seconds to wait for health (default: 60).",
    )

    subparsers.add_parser(
        "doctor", help="Report snapshot health and live-vs-newest drift."
    )

    activate = subparsers.add_parser(
        "activate", help="Idle-gate and restart one governed listener."
    )
    activate.add_argument(
        "--listener",
        choices=sorted(LISTENER_BY_NAME),
        required=True,
        help="Listener name from the recovery surface.",
    )
    activate.add_argument(
        "--idle-timeout",
        type=int,
        default=900,
        help="Seconds to wait for idle (default: 900).",
    )
    activate.add_argument(
        "--restart-timeout",
        type=int,
        default=60,
        help="Seconds to wait for health (default: 60).",
    )
    activate.add_argument(
        "--live-check",
        action="store_true",
        help=(
            "After restart, make one tiny low-effort completion through the "
            "listener's configured default model."
        ),
    )

    sanitize = subparsers.add_parser(
        "sanitize", help="Build a verified, secret-free copy for replication."
    )
    sanitize.add_argument(
        "snapshot_name", nargs="?", help="Snapshot directory name (default: newest)."
    )
    sanitize.add_argument(
        "--destination",
        type=Path,
        default=SANITIZED_ROOT,
        help="Destination root (default: %(default)s)",
    )

    return parser


def emit(value: Any, *, human: bool = False) -> None:
    """JSON on stdout, so callers (scripts, agents, launchd) can consume it."""
    print(json.dumps(value, indent=2, sort_keys=True, default=str))


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv).parse_args(argv)
    root = args.root
    if args.command == "snapshot":
        result = snapshot(label=args.label, root=root)
        emit(result)
        return 0 if result.get("verified", {}).get("ok") else 1
    if args.command == "list":
        emit(list_snapshots(root))
        return 0
    if args.command == "verify":
        name = args.snapshot_name
        if name is None:
            snapshots = sorted(
                (
                    entry
                    for entry in root.iterdir()
                    if entry.is_dir() and (entry / MANIFEST_NAME).is_file()
                ),
                key=lambda entry: entry.name,
            )
            if not snapshots:
                print(json.dumps({"ok": False, "error": "no snapshots"}))
                return 1
            name = snapshots[-1].name
        result = verify_snapshot(root / name)
        emit(result)
        return 0 if result.get("ok") else 1
    if args.command == "restore":
        name = args.snapshot_name
        if name is None:
            snapshots = sorted(
                (
                    entry
                    for entry in root.iterdir()
                    if entry.is_dir() and (entry / MANIFEST_NAME).is_file()
                ),
                key=lambda entry: entry.name,
            )
            if not snapshots:
                print(json.dumps({"ok": False, "error": "no snapshots"}))
                return 1
            name = snapshots[-1].name
        result = restore(
            root / name,
            apply=args.apply,
            restart=args.restart,
            selected=tuple(args.select),
            idle_timeout=args.idle_timeout,
            restart_timeout=args.restart_timeout,
        )
        emit(result)
        return 0 if result.get("ok") else 1
    if args.command == "doctor":
        result = doctor(root)
        emit(result)
        return 0 if result.get("ok") else 1
    if args.command == "activate":
        result = activate_listener(
            args.listener,
            idle_timeout=args.idle_timeout,
            restart_timeout=args.restart_timeout,
            live_check=args.live_check,
            root=root,
        )
        emit(result)
        return 0 if result.get("ok") else 1
    if args.command == "sanitize":
        name = args.snapshot_name
        snapshots = sorted(
            (
                entry
                for entry in root.iterdir()
                if entry.is_dir() and (entry / MANIFEST_NAME).is_file()
            ),
            key=lambda entry: entry.name,
        )
        if not snapshots:
            print(json.dumps({"ok": False, "error": "no snapshots"}))
            return 1
        source = snapshots[-1] if name is None else root / name
        result = sanitize_snapshot(source, root=root, destination_root=args.destination)
        emit(result)
        return 0 if result.get("ok") else 1
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
