import copy
import os
import tempfile
import unittest
import unittest.mock
from pathlib import Path
from typing import Any

import ccr_fallback_policy as policy


class PolicyTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.policy: dict[str, Any] = {
            "version": 1,
            "routes": ["fast,x", "fast,y", "full,z", "last,k"],
            "providers": [{"name": "last", "models": ["k"], "api_key": "${LAST_KEY}"}],
            "consumers": {
                "worker.json": {"primary": "fast,x"},
                "interactive.json": {"primary": "full,z"},
            },
        }
        self.config: dict[str, Any] = {
            "Providers": [
                {"name": "fast", "models": ["x", "y"]},
                {"name": "full", "models": ["z"]},
            ],
            "Router": {
                "default": "full,z",
                "retrySweeps": {"enabled": True, "maxHoldMs": 12345},
                "modelAliases": {"outside": "unrelated,model"},
            },
            "PORT": 1234,
        }
        self.write_policy()
        for name in self.policy["consumers"]:
            policy.atomic_json(self.root / name, self.config)

    def write_policy(self) -> None:
        policy.atomic_json(self.root / "fallback-policy.json", self.policy)

    def test_shared_edit_reaches_all_consumers_without_losing_primary_or_limits(
        self,
    ) -> None:
        policy.sync(self.root)
        self.policy["routes"][0:2] = ["fast,y", "fast,x"]
        self.write_policy()
        self.assertEqual(policy.main(["check", "--root", str(self.root), "--json"]), 1)
        result = policy.sync(self.root)
        self.assertEqual(
            result["consumers"]["interactive.json"],
            ["full,z", "fast,y", "fast,x", "last,k"],
        )
        self.assertEqual(
            result["consumers"]["worker.json"], ["fast,x", "fast,y", "full,z", "last,k"]
        )
        for name in self.policy["consumers"]:
            config = policy.read_json(self.root / name)
            self.assertEqual(config["Router"]["retrySweeps"]["maxHoldMs"], 12345)
            self.assertEqual(
                config["Router"]["modelAliases"], self.config["Router"]["modelAliases"]
            )
            self.assertEqual(config["PORT"], 1234)
        self.assertFalse(policy.sync(self.root, check=True)["changed"])

    def test_invalid_consumer_prevents_all_writes(self) -> None:
        before = (self.root / "worker.json").read_bytes()
        incompatible = copy.deepcopy(self.config)
        incompatible["Router"]["topK"] = 1
        policy.atomic_json(self.root / "interactive.json", incompatible)
        with self.assertRaises(ValueError):
            policy.sync(self.root)
        self.assertEqual((self.root / "worker.json").read_bytes(), before)
        for override in ({"sweepCooldownMs": 0}, {"maxHoldMs": -1}, {"enabled": "yes"}):
            incompatible = copy.deepcopy(self.config)
            incompatible["Router"]["retrySweeps"].update(override)
            policy.atomic_json(self.root / "interactive.json", incompatible)
            with self.assertRaises(ValueError):
                policy.sync(self.root)
            self.assertEqual((self.root / "worker.json").read_bytes(), before)
        policy.atomic_json(self.root / "interactive.json", self.config)
        self.policy["consumers"]["interactive.json"]["primary"] = "missing,model"
        self.write_policy()
        with self.assertRaises(ValueError):
            policy.sync(self.root)
        self.assertEqual((self.root / "worker.json").read_bytes(), before)

    def test_final_fallback_cannot_be_promoted_by_extra_routes(self) -> None:
        self.policy["consumers"]["worker.json"]["additional_routes"] = [
            "last,k",
            "fast,y",
        ]
        # Duplicate existing routes are harmless and cannot reorder the final fallback.
        result = policy.derive(self.config, self.policy, "worker.json")
        self.assertEqual(result["Router"]["tiers"][-1], "last,k")
        self.policy["consumers"]["worker.json"]["primary"] = "last,k"
        with self.assertRaises(ValueError):
            policy.derive(self.config, self.policy, "worker.json")

    def test_credentials_are_references_and_loaded_only_when_needed(self) -> None:
        source = copy.deepcopy(self.policy)
        source["providers"][0]["api_key"] = "literal-secret"
        with self.assertRaises(ValueError):
            policy.derive(self.config, source, "worker.json")
        path = self.root / "credentials.json"
        policy.atomic_json(
            path, {"LAST_KEY": "private-example", "UNRELATED_SECRET": "unused"}
        )
        path.chmod(0o600)
        config = policy.derive(self.config, self.policy, "worker.json")
        env = policy.credential_environment(config, path)
        self.assertEqual(env["LAST_KEY"], "private-example")
        self.assertNotIn("UNRELATED_SECRET", env)
        path.chmod(0o644)
        with self.assertRaises(ValueError):
            policy.credential_environment(config, path)
        # No file at all: inherited variables alone satisfy the references.
        with unittest.mock.patch.dict(os.environ, {"LAST_KEY": "from-env"}):
            env = policy.credential_environment(config, None)
        self.assertEqual(env["LAST_KEY"], "from-env")

    def test_path_escape_and_duplicate_chain_rejected(self) -> None:
        self.policy["consumers"]["../escape.json"] = {"primary": "fast,x"}
        self.write_policy()
        with self.assertRaises(ValueError):
            policy.load_policy(self.root)
        self.policy["consumers"].pop("../escape.json")
        # Glob metacharacters would turn the staging-file scan into a pattern.
        for name in ("wor[k].json", "glm*.json", "worker?.json"):
            self.policy["consumers"][name] = {"primary": "fast,x"}
            self.write_policy()
            with self.assertRaises(ValueError):
                policy.load_policy(self.root)
            self.policy["consumers"].pop(name)
        for reserved in ("fallback-policy.json", "runtime-credentials.json"):
            self.policy["consumers"][reserved] = {"primary": "fast,x"}
            self.write_policy()
            with self.assertRaises(ValueError):
                policy.load_policy(self.root)
            self.policy["consumers"].pop(reserved)
        self.policy["routes"].append("last,k")
        self.write_policy()
        with self.assertRaises(ValueError):
            policy.load_policy(self.root)

    def test_duplicate_provider_names_rejected(self) -> None:
        duplicated = copy.deepcopy(self.config)
        duplicated["Providers"].append({"name": "fast", "models": ["x"]})
        with self.assertRaises(ValueError):
            policy.derive(duplicated, self.policy, "worker.json")
        shared = copy.deepcopy(self.policy)
        shared["providers"].append(shared["providers"][0])
        with self.assertRaises(ValueError):
            policy.derive(self.config, shared, "worker.json")

    def test_stale_staging_report_follows_symlinked_consumers(self) -> None:
        managed = self.root / "managed"
        managed.mkdir()
        target = managed / "real.json"
        policy.atomic_json(target, self.config)
        link = self.root / "linked.json"
        link.symlink_to(target)
        self.policy["consumers"]["linked.json"] = {"primary": "fast,x"}
        self.write_policy()
        stale = managed / ".real.json.orphaned"
        stale.write_text("{}", encoding="utf-8")
        self.assertEqual(
            policy.sync(self.root, check=True)["stale_staging"],
            [str(Path(os.path.realpath(stale)))],
        )
        self.assertTrue(stale.exists())

    def test_non_list_provider_models_rejected(self) -> None:
        for broken in ("x y", {"x": 1}):
            config = copy.deepcopy(self.config)
            config["Providers"][0]["models"] = broken
            with self.assertRaises(ValueError):
                policy.derive(config, self.policy, "worker.json")

    def test_atomic_json_preserves_mode(self) -> None:
        target = self.root / "worker.json"
        target.chmod(0o640)
        policy.atomic_json(target, self.config)
        self.assertEqual(target.stat().st_mode & 0o777, 0o640)

    def test_atomic_json_first_write_mode_and_symlink_passthrough(self) -> None:
        mask = os.umask(0)
        os.umask(mask)
        managed = self.root / "managed"
        managed.mkdir()
        target = managed / "linked.json"
        link = self.root / "linked.json"
        link.symlink_to(target)
        policy.atomic_json(link, self.config)
        self.assertTrue(link.is_symlink())
        self.assertEqual(target.stat().st_mode & 0o777, 0o666 & ~mask)
        self.assertEqual(policy.read_json(target)["PORT"], 1234)

    def test_prepare_reports_stale_staging_without_deleting(self) -> None:
        stale = self.root / ".worker.json.orphaned"
        stale.write_text("{}", encoding="utf-8")
        unrelated = self.root / ".unrelated.json.bak"
        unrelated.write_text("{}", encoding="utf-8")
        _, _, reported = policy.prepare(self.root)
        self.assertEqual(reported, [str(Path(os.path.realpath(stale)))])
        self.assertTrue(stale.exists())
        self.assertTrue(unrelated.exists())
        self.assertEqual(
            policy.sync(self.root, check=True)["stale_staging"],
            [str(Path(os.path.realpath(stale)))],
        )

    def test_installed_binary_resolution(self) -> None:
        fake = self.root / "tool"
        fake.write_text("#!/bin/sh\n", encoding="utf-8")
        fake.chmod(0o755)
        with unittest.mock.patch.dict(os.environ, {"CCR_TEST_BINARY": str(fake)}):
            self.assertEqual(policy.installed_binary(), fake)
        with unittest.mock.patch.dict(
            os.environ,
            {
                "CCR_TEST_BINARY": str(self.root / "missing"),
                "CARGO_HOME": str(self.root / "empty-cargo"),
                "PATH": "",
            },
        ):
            self.assertIsNone(policy.installed_binary())

    def test_serve_preflight_and_credential_scoping(self) -> None:
        fake = self.root / "fake-ccr-rust"
        fake.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        fake.chmod(0o755)
        credentials = self.root / "runtime-credentials.json"
        policy.atomic_json(credentials, {"LAST_KEY": "private-example"})
        credentials.chmod(0o600)
        other = self.root / "unregistered.json"
        policy.atomic_json(other, self.config)
        execve = []
        with (
            unittest.mock.patch.dict(os.environ, {"LAST_KEY": "inherited"}),
            unittest.mock.patch.object(policy, "installed_binary", lambda: fake),
            unittest.mock.patch(
                "os.execve", side_effect=lambda *args: execve.append(args)
            ),
        ):
            policy.serve(other, ["start"], root=self.root)
            self.assertNotIn("private-example", execve[0][2].values())
            before = (self.root / "worker.json").read_bytes()
            policy.serve(self.root / "worker.json", ["start"], root=self.root)
            self.assertEqual(execve[1][2]["LAST_KEY"], "private-example")
            self.assertNotEqual((self.root / "worker.json").read_bytes(), before)
        with (
            unittest.mock.patch.object(policy, "installed_binary", lambda: None),
            self.assertRaises(FileNotFoundError),
        ):
            policy.serve(self.root / "worker.json", ["start"], root=self.root)
        # A credentials file that cannot resolve every referenced variable
        # fails the launch before the consumer config is rewritten.
        credentials.write_text("{}", encoding="utf-8")
        credentials.chmod(0o600)
        policy.atomic_json(self.root / "worker.json", self.config)
        before = (self.root / "worker.json").read_bytes()
        with (
            unittest.mock.patch.object(policy, "installed_binary", lambda: fake),
            unittest.mock.patch.dict(os.environ, {}, clear=True),
            unittest.mock.patch(
                "os.execve", side_effect=lambda *args: execve.append(args)
            ),
            self.assertRaises(ValueError),
        ):
            policy.serve(self.root / "worker.json", ["start"], root=self.root)
        self.assertEqual((self.root / "worker.json").read_bytes(), before)
        # Without the file, inherited variables alone satisfy the references
        # and the rewrite proceeds.
        credentials.unlink()
        with (
            unittest.mock.patch.object(policy, "installed_binary", lambda: fake),
            unittest.mock.patch.dict(os.environ, {"LAST_KEY": "inherited-only"}),
            unittest.mock.patch(
                "os.execve", side_effect=lambda *args: execve.append(args)
            ),
        ):
            policy.serve(self.root / "worker.json", ["start"], root=self.root)
        self.assertEqual(execve[-1][2]["LAST_KEY"], "inherited-only")
        self.assertNotEqual((self.root / "worker.json").read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
