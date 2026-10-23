"""Tests for the CCR configuration recovery tool.

Every test runs against a synthetic filesystem; none of them read or write the
operator's live routing surface.
"""

from __future__ import annotations

import hashlib
import json
import os
import stat
import subprocess
import tempfile
import threading
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any, Self
from unittest import mock

import ccr_recovery as recovery


class RecoveryTests(unittest.TestCase):
    def setUp(self) -> None:
        cache_root = Path(
            os.environ.get("ALPHAHENG_CACHE_ROOT", Path.home() / ".cache" / "alphaheng")
        )
        cache_root.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(
            prefix="ccr-recovery-test-", dir=cache_root
        )
        self.addCleanup(self.directory.cleanup)
        self.base = Path(self.directory.name)
        self.config_root = self.base / "claude-code-router"
        self.launch_agents = self.base / "LaunchAgents"
        self.local_bin = self.base / "bin"
        self.cloudflared = self.base / "cloudflared"
        self.syncthing_config = self.base / "Syncthing" / "config.xml"
        self.toolbelt_manifests = self.base / "toolbelt-manifests"
        self.lapis_tools = self.base / "lapis-tools"
        self.snapshots = self.base / "snapshots"
        for directory in (
            self.config_root,
            self.launch_agents,
            self.local_bin,
            self.cloudflared,
        ):
            directory.mkdir(parents=True)

        self.config = {
            "Providers": [{"name": "zai", "api_key": "${CCR_ZAI_API_KEY}"}],
            "Router": {"default": "zai,glm-5.3", "tiers": ["zai,glm-5.3"]},
        }
        (self.config_root / "config.json").write_text(json.dumps(self.config))
        (self.config_root / "runtime-credentials.json").write_text(
            json.dumps({"CCR_ZAI_API_KEY": "secret"})
        )
        os.chmod(self.config_root / "runtime-credentials.json", 0o600)
        (self.launch_agents / "com.kearm.ccr-rust.plist").write_text("<plist/>")
        self.syncthing_config.parent.mkdir(parents=True)
        self.syncthing_config.write_text(
            "<configuration><apikey>secret</apikey></configuration>"
        )
        self.toolbelt_manifests.mkdir()
        self.lapis_tools.mkdir()
        self.share_root = self.base / "share"
        self.share_root.mkdir()
        for name in recovery.TOOL_MANIFESTS:
            (self.toolbelt_manifests / name).write_text('{"name": "tool"}')
            (self.lapis_tools / name).write_text('{"name": "tool"}')
        (self.local_bin / "ccr-local").write_text("#!/usr/bin/env bash\n")
        (self.local_bin / "ccr-recovery").write_text("# recovery wrapper\n")
        (self.share_root / "ccr_fallback_policy.py").write_text("# policy\n")
        (self.share_root / "ccr_recovery.py").write_text("# recovery\n")
        (self.local_bin / "ccr-fallback-policy").symlink_to(
            self.base / "deployed" / "ccr_fallback_policy.py"
        )
        (self.base / "deployed").mkdir()
        (self.base / "deployed" / "ccr_fallback_policy.py").write_text("# policy\n")

        self.patches = [
            mock.patch.object(recovery, "CONFIG_ROOT", self.config_root),
            mock.patch.object(recovery, "LAUNCH_AGENTS", self.launch_agents),
            mock.patch.object(recovery, "LOCAL_BIN", self.local_bin),
            mock.patch.object(recovery, "CLOUDFLARED", self.cloudflared),
            mock.patch.object(recovery, "SYNCTHING_CONFIG", self.syncthing_config),
            mock.patch.object(recovery, "TOOLBELT_MANIFESTS", self.toolbelt_manifests),
            mock.patch.object(recovery, "LAPIS_TOOL_MANIFESTS", self.lapis_tools),
            mock.patch.object(recovery, "SNAPSHOT_ROOT", self.snapshots),
            mock.patch.object(recovery, "SHARE_ROOT", self.share_root),
        ]
        for patch in self.patches:
            patch.start()
            self.addCleanup(patch.stop)
        # Only the labels this fixture creates, so the tool does not expect
        # files the fixture never made.
        mock.patch.object(
            recovery, "LAUNCH_AGENT_LABELS", ("com.kearm.ccr-rust",)
        ).start()
        mock.patch.object(
            recovery,
            "BIN_FILES",
            ("ccr-local", "ccr-fallback-policy", "ccr-recovery"),
        ).start()
        mock.patch.object(
            recovery, "SHARE_FILES", ("ccr_fallback_policy.py", "ccr_recovery.py")
        ).start()
        mock.patch.object(
            recovery, "CONFIG_FILES", ("config.json", "runtime-credentials.json")
        ).start()
        mock.patch.object(recovery, "SHARE_ROOT", self.base / "share").start()
        self.codex_home = self.base / "codex"
        self.codex_home.mkdir()
        (self.codex_home / "ccr-live-model-catalog.json").write_text('{"models": []}')
        mock.patch.object(recovery, "CODEX_HOME", self.codex_home).start()
        mock.patch.object(recovery, "runtime_listener_checks", return_value=[]).start()
        self.addCleanup(mock.patch.stopall)

    # --- snapshot ---------------------------------------------------------

    def test_snapshot_captures_surface_and_verifies(self) -> None:
        result = recovery.snapshot(label="test", root=self.snapshots)
        self.assertTrue(result["verified"]["ok"], result["verified"])
        self.assertEqual(result["label"], "test")
        self.assertIn("config-root/config.json", result["captured"])
        self.assertIn("launch-agents/com.kearm.ccr-rust.plist", result["captured"])
        self.assertTrue(
            (self.snapshots / result["snapshot"] / "manifest.json").is_file()
        )

    def test_snapshot_captures_codex_model_catalog(self) -> None:
        result = recovery.snapshot(label="test", root=self.snapshots)
        self.assertIn("codex/ccr-live-model-catalog.json", result["captured"])

    def test_snapshot_captures_but_never_replicates_syncthing_config(self) -> None:
        result = recovery.snapshot(label="test", root=self.snapshots)
        self.assertIn("syncthing/config.xml", result["captured"])
        destination = self.base / "shared"
        sanitized = recovery.sanitize_snapshot(
            self.snapshots / result["snapshot"],
            root=self.snapshots,
            destination_root=destination,
        )
        self.assertTrue(sanitized["ok"])
        self.assertIn("syncthing/config.xml", sanitized["omitted"])
        self.assertFalse(
            (destination / result["snapshot"] / "syncthing/config.xml").exists()
        )

    def test_sanitize_fails_closed_on_credential_shaped_content(self) -> None:
        config_files: list[str] = [*recovery.CONFIG_FILES, "serve.py"]
        with mock.patch.object(recovery, "CONFIG_FILES", config_files):
            (self.config_root / "serve.py").write_text(
                'API_KEY = "abcdefghijklmnopqrstuvwxyz"\n', encoding="utf-8"
            )
            snapshot_result = recovery.snapshot(label="test", root=self.snapshots)

        destination = self.base / "shared"
        result = recovery.sanitize_snapshot(
            self.snapshots / snapshot_result["snapshot"],
            root=self.snapshots,
            destination_root=destination,
        )
        self.assertFalse(result["ok"])
        self.assertEqual(result["stage"], "secret-scan")
        self.assertFalse((destination / snapshot_result["snapshot"]).exists())

    def test_snapshot_captures_launchd_entrypoint_scripts(self) -> None:
        for name in recovery.LAUNCHER_FILES:
            (self.config_root / name).write_text("# launcher\n")
        result = recovery.snapshot(label="test", root=self.snapshots)
        self.assertIn("config-root/serve.py", result["captured"])
        self.assertIn("config-root/serve-glm-workers.py", result["captured"])
        self.assertIn("config-root/shape_probe_auth.py", result["captured"])

    def test_snapshot_captures_both_tool_registration_copies(self) -> None:
        result = recovery.snapshot(label="test", root=self.snapshots)
        captured = set(result["captured"])
        self.assertIn("toolbelt-manifests/ccr-recovery.tool.json", captured)
        self.assertIn("toolbelt-manifests/ccr-catalog-sync.tool.json", captured)
        self.assertIn("lapis-tools/ccr-recovery.tool.json", captured)
        self.assertIn("lapis-tools/ccr-catalog-sync.tool.json", captured)

    def test_snapshot_captures_the_recovery_tool_implementation(self) -> None:
        result = recovery.snapshot(label="test", root=self.snapshots)
        captured = set(result["captured"])
        self.assertIn("local-bin/ccr-recovery", captured)
        self.assertIn("share/ccr_recovery.py", captured)

    def test_snapshot_root_is_private(self) -> None:
        recovery.snapshot(label="test", root=self.snapshots)
        self.assertEqual(stat.S_IMODE(self.snapshots.stat().st_mode), 0o700)
        for snapshot in self.snapshots.iterdir():
            self.assertEqual(stat.S_IMODE(snapshot.stat().st_mode), 0o700)

    def test_snapshot_preserves_secret_file_mode(self) -> None:
        result = recovery.snapshot(label="test", root=self.snapshots)
        manifest = recovery.read_manifest(self.snapshots / result["snapshot"])
        self.assertEqual(
            manifest["files"]["config-root/runtime-credentials.json"]["mode"], 0o600
        )

    def test_snapshot_records_symlink_target_separately(self) -> None:
        result = recovery.snapshot(label="test", root=self.snapshots)
        captured = set(result["captured"])
        self.assertIn("local-bin/ccr-fallback-policy", captured)
        self.assertIn("local-bin-targets/ccr-fallback-policy", captured)

    def test_snapshot_captures_dangling_symlink(self) -> None:
        bin_files: list[str] = [*recovery.BIN_FILES, "dangling"]
        with mock.patch.object(recovery, "BIN_FILES", bin_files):
            (self.local_bin / "dangling").symlink_to(self.base / "missing-target")
            result = recovery.snapshot(label="test", root=self.snapshots)
        self.assertIn("local-bin/dangling", result["captured"])
        self.assertNotIn("local-bin/dangling", result["missing"])

    def test_snapshot_reports_missing_files_without_failing(self) -> None:
        (self.local_bin / "ccr-recovery").unlink()
        result = recovery.snapshot(label="test", root=self.snapshots)
        self.assertTrue(result["verified"]["ok"])
        self.assertIn("local-bin/ccr-recovery", result["missing"])

    def test_snapshot_is_staged_so_interruption_leaves_no_partial_directory(
        self,
    ) -> None:
        def explode(*_args, **_kwargs):
            raise OSError("disk full")

        with (
            mock.patch.object(recovery, "write_manifest", explode),
            self.assertRaises(OSError),
        ):
            recovery.snapshot(label="test", root=self.snapshots)
        self.assertEqual(
            [entry for entry in self.snapshots.iterdir() if entry.is_dir()], []
        )

    def test_concurrent_snapshot_claims_are_exclusive(self) -> None:
        barrier = threading.Barrier(4)

        def capture(index: int) -> dict[str, Any]:
            barrier.wait(timeout=5)
            return recovery.snapshot(label=f"concurrent-{index}", root=self.snapshots)

        with ThreadPoolExecutor(max_workers=4) as pool:
            results = list(pool.map(capture, range(4)))
        names = [result["snapshot"] for result in results]
        self.assertEqual(len(set(names)), 4, names)
        self.assertTrue(all(result["verified"]["ok"] for result in results), results)

    def test_prune_keeps_newest_and_reports_removed(self) -> None:
        names = []
        for index in range(recovery.KEEP_SNAPSHOTS + 3):
            result = recovery.snapshot(label=f"s{index}", root=self.snapshots)
            names.append(result["snapshot"])
        remaining = sorted(entry.name for entry in self.snapshots.iterdir())
        self.assertEqual(len(remaining), recovery.KEEP_SNAPSHOTS)
        # A suffix disambiguates same-second snapshots; only later snapshots in
        # one second get one, so the unsuffixed stem identifies the newest.
        self.assertTrue(remaining[-1].startswith(names[-1]))

    # --- verify -----------------------------------------------------------

    def test_verify_rejects_directory_without_manifest(self) -> None:
        orphan = self.snapshots / "20260101T000000+0000"
        orphan.mkdir(parents=True)
        result = recovery.verify_snapshot(orphan)
        self.assertFalse(result["ok"])
        self.assertEqual(result["error"], "no manifest")

    def test_verify_detects_tampered_file(self) -> None:
        result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / result["snapshot"]
        (snapshot / "config-root" / "config.json").write_text('{"tampered": true}')
        verification = recovery.verify_snapshot(snapshot)
        self.assertFalse(verification["ok"])
        self.assertIn("config-root/config.json", verification["mismatched"])

    def test_verify_detects_deleted_file(self) -> None:
        result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / result["snapshot"]
        (snapshot / "config-root" / "config.json").unlink()
        verification = recovery.verify_snapshot(snapshot)
        self.assertFalse(verification["ok"])
        self.assertIn("config-root/config.json", verification["missing"])

    def test_verify_detects_unsupported_manifest_version(self) -> None:
        result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / result["snapshot"]
        manifest_path = snapshot / recovery.MANIFEST_NAME
        manifest = json.loads(manifest_path.read_text())
        manifest["manifest_version"] = 99
        manifest_path.write_text(json.dumps(manifest))
        verification = recovery.verify_snapshot(snapshot)
        self.assertFalse(verification["ok"])
        self.assertIn("Unsupported manifest version", verification["error"])

    def test_verify_rejects_manifest_path_escape(self) -> None:
        snapshot_result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / snapshot_result["snapshot"]
        manifest_path = snapshot / recovery.MANIFEST_NAME
        manifest = json.loads(manifest_path.read_text())
        record = next(iter(manifest["files"].values()))
        manifest["files"]["../escape"] = record
        manifest_path.write_text(json.dumps(manifest))

        verification = recovery.verify_snapshot(snapshot)
        self.assertFalse(verification["ok"])
        self.assertIn("Unsafe snapshot path", verification["error"])

    def test_verify_rejects_unsafe_symlink_target(self) -> None:
        snapshot_result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / snapshot_result["snapshot"]
        manifest_path = snapshot / recovery.MANIFEST_NAME
        manifest = json.loads(manifest_path.read_text())
        name = "config-root/config.json"
        manifest["files"][name]["symlink"] = True
        payload = "/outside/home/target\n"
        (snapshot / name).write_text(payload, encoding="utf-8")
        manifest["files"][name]["sha256"] = hashlib.sha256(payload.encode()).hexdigest()
        manifest_path.write_text(json.dumps(manifest))

        self.assertFalse(recovery.safe_symlink_target("/outside/home/target"))
        verification = recovery.verify_snapshot(snapshot)
        self.assertFalse(verification["ok"])
        self.assertIn(name, verification["mismatched"])

    # --- restore ----------------------------------------------------------

    def test_restore_defaults_to_dry_run(self) -> None:
        snapshot_result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / snapshot_result["snapshot"]
        original = (self.config_root / "config.json").read_text()
        (self.config_root / "config.json").write_text('{"broken": true}')

        result = recovery.restore(snapshot)

        self.assertEqual(result["stage"], "dry-run")
        states = {change["relative"]: change["state"] for change in result["changes"]}
        self.assertEqual(states["config-root/config.json"], "modified")
        # Nothing was written: the damaged file is still damaged.
        self.assertEqual(
            (self.config_root / "config.json").read_text(), '{"broken": true}'
        )
        self.assertNotEqual(original, '{"broken": true}')

    def test_restore_apply_recovers_damaged_config(self) -> None:
        snapshot_result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / snapshot_result["snapshot"]
        good = (self.config_root / "config.json").read_text()
        (self.config_root / "config.json").write_text('{"broken": true}')

        result = recovery.restore(snapshot, apply=True)

        self.assertTrue(result["ok"], result)
        self.assertTrue(result["validation"]["ok"])
        self.assertEqual((self.config_root / "config.json").read_text(), good)

    def test_restore_apply_refuses_corrupt_snapshot(self) -> None:
        snapshot_result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / snapshot_result["snapshot"]
        (snapshot / "config-root" / "config.json").write_text('{"tampered": true}')
        before = (self.config_root / "config.json").read_text()

        result = recovery.restore(snapshot, apply=True)

        self.assertFalse(result["ok"])
        self.assertEqual(result["stage"], "verify")
        self.assertEqual((self.config_root / "config.json").read_text(), before)

    def test_restore_restores_symlink_as_symlink(self) -> None:
        snapshot_result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / snapshot_result["snapshot"]
        link = self.local_bin / "ccr-fallback-policy"
        original_target = os.readlink(link)
        link.unlink()
        link.write_text("# replaced with a regular file\n")

        recovery.restore(snapshot, apply=True)

        self.assertTrue(link.is_symlink())
        self.assertEqual(os.readlink(link), original_target)

    def test_restore_restores_the_file_behind_a_drifted_link(self) -> None:
        snapshot_result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / snapshot_result["snapshot"]
        link = self.local_bin / "ccr-fallback-policy"
        target = Path(os.readlink(link))
        good = target.read_text()
        target.write_text("# clobbered target\n", encoding="utf-8")

        result = recovery.restore(snapshot, apply=True)

        self.assertTrue(result["ok"], result)
        self.assertTrue(link.is_symlink())
        self.assertEqual(os.readlink(link), str(target))
        self.assertEqual(target.read_text(), good)

    def test_restore_dry_run_does_not_flag_a_healthy_symlink(self) -> None:
        snapshot_result = recovery.snapshot(label="test", root=self.snapshots)
        result = recovery.restore(self.snapshots / snapshot_result["snapshot"])
        states = {change["relative"]: change["state"] for change in result["changes"]}
        self.assertEqual(states["local-bin/ccr-fallback-policy"], "unchanged")
        self.assertFalse(result["drift"])

    def test_restore_replaces_drifted_symlink_with_regular_file(self) -> None:
        snapshot_result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / snapshot_result["snapshot"]
        target = self.base / "attacker-controlled"
        target.write_text("not the snapshot", encoding="utf-8")
        link = self.local_bin / "ccr-local"
        good = link.read_text()
        link.unlink()
        link.symlink_to(target)

        dry_run = recovery.restore(snapshot)
        states = {change["relative"]: change["state"] for change in dry_run["changes"]}
        self.assertEqual(states["local-bin/ccr-local"], "type-drift")

        result = recovery.restore(snapshot, apply=True)

        self.assertTrue(result["ok"], result)
        self.assertFalse(link.is_symlink())
        self.assertEqual(link.read_text(), good)

    def test_retired_claude_ccr_is_not_restored(self) -> None:
        """A pre-retirement snapshot must not put the old launcher back."""
        retired = self.local_bin / "claude-ccr"
        retired.write_text(
            "#!/bin/bash\nexport ANTHROPIC_BASE_URL=http://127.0.0.1:3456\n"
        )
        os.chmod(retired, 0o700)

        # Capture the surface while the pre-retirement launcher is present.
        with mock.patch.object(
            recovery, "BIN_FILES", ("ccr-local", "ccr-fallback-policy", "claude-ccr")
        ):
            snapshot_result = recovery.snapshot(
                label="pre-retirement", root=self.snapshots
            )
        snapshot = self.snapshots / snapshot_result["snapshot"]
        self.assertIn("local-bin/claude-ccr", snapshot_result["captured"])

        # The machine now has the retired stub instead.
        retired.write_text("#!/usr/bin/env bash\nexit 2\n")
        os.chmod(retired, 0o755)

        result = recovery.restore(snapshot, apply=True)

        self.assertIn("local-bin/claude-ccr", result["unmapped"])
        relatives = {change["relative"] for change in result["changes"]}
        self.assertNotIn("local-bin/claude-ccr", relatives)
        self.assertEqual(retired.read_text(), "#!/usr/bin/env bash\nexit 2\n")

    def test_retired_claude_ccr_symlink_target_is_not_restored(self) -> None:
        """The symlink-target form of a retired wrapper is also not restored."""
        deployed = self.base / "deployed" / "claude-ccr"
        deployed.write_text("#!/bin/bash\nexit 0\n")
        retired = self.local_bin / "claude-ccr"
        retired.symlink_to(deployed)

        with mock.patch.object(
            recovery, "BIN_FILES", ("ccr-local", "ccr-fallback-policy", "claude-ccr")
        ):
            snapshot_result = recovery.snapshot(
                label="pre-retirement", root=self.snapshots
            )
        snapshot = self.snapshots / snapshot_result["snapshot"]
        self.assertIn("local-bin/claude-ccr", snapshot_result["captured"])

        retired.unlink()
        result = recovery.restore(snapshot, apply=True)

        self.assertIn("local-bin/claude-ccr", result["unmapped"])
        self.assertIn("local-bin-targets/claude-ccr", result["unmapped"])
        self.assertFalse(retired.exists())

    def test_restore_reports_unmapped_retired_manifest_entries(self) -> None:
        snapshot_result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / snapshot_result["snapshot"]
        manifest_path = snapshot / recovery.MANIFEST_NAME
        manifest = json.loads(manifest_path.read_text())
        record = dict(manifest["files"]["config-root/config.json"])
        payload = "{}\n"
        (snapshot / "config-root" / "retired.json").write_text(payload)
        record["sha256"] = hashlib.sha256(payload.encode()).hexdigest()
        manifest["files"]["config-root/retired.json"] = record
        manifest_path.write_text(json.dumps(manifest))

        result = recovery.restore(snapshot)

        self.assertEqual(result["stage"], "dry-run", result)
        self.assertIn("config-root/retired.json", result["unmapped"])
        relatives = {change["relative"] for change in result["changes"]}
        self.assertNotIn("config-root/retired.json", relatives)

    def test_restore_restores_secret_file_mode(self) -> None:
        snapshot_result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / snapshot_result["snapshot"]
        credentials = self.config_root / "runtime-credentials.json"
        os.chmod(credentials, 0o644)

        recovery.restore(snapshot, apply=True)

        self.assertEqual(stat.S_IMODE(credentials.stat().st_mode), 0o600)

    def test_restore_selected_limits_scope(self) -> None:
        snapshot_result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / snapshot_result["snapshot"]
        good = (self.config_root / "config.json").read_text()
        (self.config_root / "config.json").write_text('{"broken": true}')
        (self.local_bin / "ccr-local").write_text("# clobbered\n")

        result = recovery.restore(
            snapshot, apply=True, selected=(str(self.config_root / "config.json"),)
        )

        self.assertTrue(result["ok"], result)
        self.assertEqual((self.config_root / "config.json").read_text(), good)
        self.assertEqual((self.local_bin / "ccr-local").read_text(), "# clobbered\n")

    def test_restore_reports_missing_live_file_as_drift(self) -> None:
        snapshot_result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / snapshot_result["snapshot"]
        (self.config_root / "config.json").unlink()

        result = recovery.restore(snapshot)

        states = {change["relative"]: change["state"] for change in result["changes"]}
        self.assertEqual(states["config-root/config.json"], "missing")

    def test_restore_recreates_deleted_live_file_on_apply(self) -> None:
        snapshot_result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / snapshot_result["snapshot"]
        good = (self.config_root / "config.json").read_text()
        (self.config_root / "config.json").unlink()

        result = recovery.restore(snapshot, apply=True)

        self.assertTrue(result["ok"], result)
        self.assertEqual((self.config_root / "config.json").read_text(), good)

    def test_restore_without_restart_does_not_touch_launchd(self) -> None:
        snapshot_result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / snapshot_result["snapshot"]
        with mock.patch.object(recovery.subprocess, "run") as run:
            recovery.restore(snapshot, apply=True)
        self.assertEqual(run.call_count, 0)

    def test_restore_with_restart_is_idle_gated(self) -> None:
        snapshot_result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / snapshot_result["snapshot"]
        with (
            mock.patch.object(recovery, "wait_idle", return_value={"ok": True}) as idle,
            mock.patch.object(recovery, "listener_healthy", return_value=True),
            mock.patch.object(
                recovery, "listener_image", return_value={"ok": True, "pid": 42}
            ),
            mock.patch.object(recovery.subprocess, "run") as run,
        ):
            run.return_value = mock.Mock(returncode=0, stdout="", stderr="")
            result = recovery.restore(snapshot, apply=True, restart=True)
        self.assertTrue(idle.called)
        self.assertEqual(result["stage"], "applied")
        self.assertTrue(result["ok"], result)
        self.assertTrue(all(item["ok"] for item in result["restarted"]))
        self.assertIn(
            "com.kearm.ccr-ocr", [item["label"] for item in result["restarted"]]
        )

    def test_restore_does_not_restart_when_a_listener_is_busy(self) -> None:
        snapshot_result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / snapshot_result["snapshot"]
        with (
            mock.patch.object(
                recovery, "wait_idle", return_value={"ok": False, "error": "busy"}
            ),
            mock.patch.object(recovery.subprocess, "run") as run,
        ):
            result = recovery.restore(snapshot, apply=True, restart=True)
        self.assertEqual(run.call_count, 0)
        self.assertFalse(result["ok"])
        self.assertEqual(result["restarted"][0]["stage"], "idle")

    # --- doctor -----------------------------------------------------------

    def test_doctor_flags_live_drift_against_newest_snapshot(self) -> None:
        recovery.snapshot(label="test", root=self.snapshots)
        (self.config_root / "config.json").write_text('{"drifted": true}')
        result = recovery.doctor(self.snapshots)
        self.assertTrue(result["ok"])
        self.assertIn("config-root/config.json", result["drifted_vs_live"])

    def test_doctor_does_not_flag_healthy_symlink_as_drift(self) -> None:
        # Hashing a symlink follows it, so a naive comparison always reports
        # drift for every link. Compare the link target path instead.
        recovery.snapshot(label="test", root=self.snapshots)
        result = recovery.doctor(self.snapshots)
        self.assertNotIn("local-bin/ccr-fallback-policy", result["drifted_vs_live"])

    def test_doctor_flags_symlink_replaced_by_regular_file(self) -> None:
        recovery.snapshot(label="test", root=self.snapshots)
        link = self.local_bin / "ccr-fallback-policy"
        target = os.readlink(link)
        link.unlink()
        link.write_text("# policy\n")
        result = recovery.doctor(self.snapshots)
        self.assertIn("local-bin/ccr-fallback-policy", result["drifted_vs_live"])
        self.assertTrue(target)

    def test_doctor_without_snapshots_is_not_ok(self) -> None:
        result = recovery.doctor(self.snapshots)
        self.assertFalse(result["ok"])

    def test_doctor_reports_malformed_newest_manifest_without_crashing(self) -> None:
        snapshot_result = recovery.snapshot(label="test", root=self.snapshots)
        snapshot = self.snapshots / snapshot_result["snapshot"]
        manifest_path = snapshot / recovery.MANIFEST_NAME
        manifest = json.loads(manifest_path.read_text())
        manifest["manifest_version"] = 99
        manifest_path.write_text(json.dumps(manifest))

        result = recovery.doctor(self.snapshots)

        self.assertFalse(result["ok"])
        self.assertIn("Unsupported manifest version", result["newest"]["error"])

    def test_doctor_fails_when_a_listener_has_stale_routes(self) -> None:
        recovery.snapshot(label="test", root=self.snapshots)
        stale = {
            "name": "main",
            "port": 3456,
            "ok": False,
            "error": "listener has a stale in-memory tier order",
        }
        with mock.patch.object(
            recovery, "runtime_listener_checks", return_value=[stale]
        ):
            result = recovery.doctor(self.snapshots)
        self.assertFalse(result["ok"])
        self.assertFalse(result["runtime_ok"])
        self.assertEqual(result["runtime_listeners"], [stale])

    def test_runtime_check_compares_loaded_tiers_to_consumer_config(self) -> None:
        with (
            mock.patch.object(
                recovery,
                "LISTENERS",
                (("main", 3456, "com.kearm.ccr-rust", "config.json"),),
            ),
            mock.patch.object(
                recovery,
                "listener_runtime",
                return_value={"ok": True, "tiers": ["old,tier"], "default": "old,tier"},
            ) as runtime,
        ):
            check = recovery.runtime_listener_check("main", 3456, "config.json")
        self.assertFalse(check["ok"])
        self.assertEqual(check["error"], "listener has a stale in-memory tier order")
        self.assertEqual(check["expected_tiers"], ["zai,glm-5.3"])
        runtime.assert_called_once_with(3456)

    def test_listener_runtime_requests_details_and_flags_old_contract(self) -> None:
        class Response:
            def __enter__(self) -> Self:
                return self

            def __exit__(self, *_args: object) -> None:
                return None

            def read(self) -> bytes:
                return b"ok"

        with mock.patch("urllib.request.urlopen", return_value=Response()) as urlopen:
            result = recovery.listener_runtime(3456)
        self.assertFalse(result["ok"])
        self.assertIn("older content-free", result["error"])
        self.assertIn("/health?details=1", urlopen.call_args.args[0])

    def test_activate_listener_uses_the_configured_launchd_label(self) -> None:
        ready = {
            "snapshot": "ready",
            "path": str(self.snapshots / "ready"),
            "created_at": "2026-10-22T14:00:00-07:00",
            "verified": {"ok": True},
            "missing": [],
        }
        with (
            mock.patch.object(recovery, "snapshot", return_value=ready),
            mock.patch.object(
                recovery,
                "restart_listener",
                return_value={"ok": True, "stage": "restarted"},
            ) as restart,
            mock.patch.object(
                recovery,
                "runtime_listener_check",
                return_value={"ok": True, "name": "ocr", "tiers": []},
            ),
        ):
            result = recovery.activate_listener("ocr")
        self.assertTrue(result["ok"])
        self.assertEqual(result["port"], 3458)
        restart.assert_called_once_with("com.kearm.ccr-ocr", 3458, 60, 900)
        self.assertTrue(result["snapshot"]["verified"]["ok"])

    def test_activate_snapshots_before_restart_and_blocks_on_bad_snapshot(self) -> None:
        bad_snapshot = {"verified": {"ok": False}}
        with (
            mock.patch.object(recovery, "snapshot", return_value=bad_snapshot),
            mock.patch.object(recovery, "restart_listener") as restart,
        ):
            result = recovery.activate_listener("main")
        self.assertFalse(result["ok"])
        self.assertEqual(result["stage"], "snapshot")
        restart.assert_not_called()

    def test_activate_blocks_when_snapshot_inventory_is_incomplete(self) -> None:
        incomplete_snapshot = {
            "verified": {"ok": True},
            "missing": ["local-bin/ccr-recovery"],
        }
        with (
            mock.patch.object(recovery, "snapshot", return_value=incomplete_snapshot),
            mock.patch.object(recovery, "restart_listener") as restart,
        ):
            result = recovery.activate_listener("main")
        self.assertFalse(result["ok"])
        self.assertEqual(result["stage"], "snapshot")
        restart.assert_not_called()

    def test_activate_honors_the_requested_snapshot_root(self) -> None:
        alternate_root = self.snapshots / "alternate"
        ready = {
            "snapshot": "ready",
            "path": str(alternate_root / "ready"),
            "created_at": "2026-10-22T14:00:00-07:00",
            "verified": {"ok": True},
            "missing": [],
        }
        with (
            mock.patch.object(recovery, "snapshot", return_value=ready) as snapshot,
            mock.patch.object(recovery, "restart_listener"),
        ):
            recovery.activate_listener("main", root=alternate_root)
        snapshot.assert_called_once_with(label="pre-activate-main", root=alternate_root)

    def test_activate_can_require_a_representative_completion(self) -> None:
        ready = {
            "snapshot": "ready",
            "path": str(self.snapshots / "ready"),
            "created_at": "2026-10-22T14:00:00-07:00",
            "verified": {"ok": True},
            "missing": [],
        }
        with (
            mock.patch.object(recovery, "snapshot", return_value=ready),
            mock.patch.object(
                recovery,
                "restart_listener",
                return_value={"ok": True, "stage": "restarted"},
            ),
            mock.patch.object(
                recovery,
                "runtime_listener_check",
                return_value={"ok": True, "name": "main", "tiers": []},
            ),
            mock.patch.object(
                recovery,
                "representative_listener_request",
                return_value={"ok": True, "content": "OK"},
            ) as live,
        ):
            result = recovery.activate_listener("main", live_check=True)
        self.assertTrue(result["ok"])
        self.assertTrue(result["live_request"]["ok"])
        live.assert_called_once_with(3456)

    def test_representative_request_rejects_malformed_choice(self) -> None:
        class Response:
            def __enter__(self) -> Self:
                return self

            def __exit__(self, *_args: object) -> None:
                return None

            def read(self) -> bytes:
                return b'{"choices":[null]}'

        with (
            mock.patch.object(
                recovery, "listener_runtime", return_value={"ok": True, "default": "m"}
            ),
            mock.patch("urllib.request.urlopen", return_value=Response()),
        ):
            result = recovery.representative_listener_request(3456)
        self.assertFalse(result["ok"])
        self.assertEqual(result["error"], "completion response has no valid choice")

    def test_representative_request_returns_scalar_finish_reason(self) -> None:
        class Response:
            def __enter__(self) -> Self:
                return self

            def __exit__(self, *_args: object) -> None:
                return None

            def read(self) -> bytes:
                return (
                    b'{"model":"m","choices":[{"message":{"content":"OK"},'
                    b'"finish_reason":"stop"}]}'
                )

        with (
            mock.patch.object(
                recovery, "listener_runtime", return_value={"ok": True, "default": "m"}
            ),
            mock.patch("urllib.request.urlopen", return_value=Response()),
        ):
            result = recovery.representative_listener_request(3456)
        self.assertTrue(result["ok"])
        self.assertEqual(result["content"], "OK")
        self.assertEqual(result["finish_reason"], "stop")

    def test_process_text_mappings_parses_lsof_fields(self) -> None:
        output = "p42\nfcwd\ni1\nn/tmp\nftxt\ni2\nn/Users/x/ccr-rust\n"
        with mock.patch.object(
            recovery.subprocess,
            "run",
            return_value=mock.Mock(returncode=0, stdout=output, stderr=""),
        ):
            self.assertEqual(
                recovery.process_text_mappings(42), [("/Users/x/ccr-rust", 2)]
            )

    def test_listener_image_requires_the_installed_vnode(self) -> None:
        binary = self.base / "ccr-rust"
        binary.write_bytes(b"binary")
        with (
            mock.patch.object(recovery, "CCR_BINARY", binary),
            mock.patch.object(recovery, "listener_pid", return_value=42),
            mock.patch.object(
                recovery,
                "process_text_mappings",
                return_value=[(str(binary), binary.stat().st_ino)],
            ),
        ):
            current = recovery.listener_image(3456)
        self.assertTrue(current["ok"], current)
        with (
            mock.patch.object(recovery, "CCR_BINARY", binary),
            mock.patch.object(recovery, "listener_pid", return_value=42),
            mock.patch.object(
                recovery,
                "process_text_mappings",
                return_value=[(str(binary), binary.stat().st_ino + 1)],
            ),
        ):
            stale = recovery.listener_image(3456)
        self.assertFalse(stale["ok"])
        self.assertIn("replaced executable inode", stale["error"])

    def test_timestamp_helpers_run_on_the_launchd_system_python(self) -> None:
        interpreter = Path("/usr/bin/python3")
        if not interpreter.is_file():
            self.skipTest("macOS system Python unavailable")
        source = Path(__file__).with_name("ccr_recovery.py")
        completed = subprocess.run(
            [
                str(interpreter),
                "-c",
                (
                    "import os, sys; sys.path.insert(0, os.environ['SCRIPTS']); "
                    "import ccr_recovery as recovery; recovery.stamp()"
                ),
            ],
            capture_output=True,
            text=True,
            check=False,
            timeout=10,
            cwd=source.parent.parent,
            env={
                "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
                "SCRIPTS": str(source.parent),
            },
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)

    def test_doctor_reports_missing_live_files(self) -> None:
        recovery.snapshot(label="test", root=self.snapshots)
        (self.config_root / "runtime-credentials.json").unlink()
        result = recovery.doctor(self.snapshots)
        self.assertIn(
            "config-root/runtime-credentials.json", result["missing_from_live"]
        )

    # --- validation -------------------------------------------------------

    def test_validate_configs_flags_invalid_json(self) -> None:
        (self.config_root / "config.json").write_text("{not json")
        result = recovery.validate_configs()
        self.assertFalse(result["ok"])

    def test_validate_configs_reports_absent_file(self) -> None:
        result = recovery.validate_configs()
        states = {item["file"]: item for item in result["configs"]}
        self.assertEqual(states["config-auth.json"]["error"], "absent")

    # --- pre_snapshot hook ------------------------------------------------

    def test_pre_snapshot_swallows_oserror_and_reports_it(self) -> None:
        with mock.patch.object(
            recovery, "snapshot", side_effect=OSError("permission denied")
        ):
            result = recovery.pre_snapshot(self.snapshots)
        self.assertFalse(result["ok"])
        self.assertIn("permission denied", result["error"])

    def test_pre_snapshot_returns_snapshot_on_success(self) -> None:
        result = recovery.pre_snapshot(self.snapshots)
        self.assertTrue(result["verified"]["ok"])

    def test_codex_profile_files_are_captured(self) -> None:
        captured = {name for _, name in recovery.relative_targets()}
        self.assertIn("codex/ccr.config.toml", captured)
        self.assertIn("codex/claude-code-router.config.toml", captured)
        self.assertIn("codex/ccr-roles/glm-worker.toml", captured)


if __name__ == "__main__":
    unittest.main()
