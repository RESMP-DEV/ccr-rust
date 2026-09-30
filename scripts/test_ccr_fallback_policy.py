import copy
import tempfile
import unittest
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
        config = policy.derive(self.config, self.policy, "worker.json")
        env = policy.credential_environment(config, path)
        self.assertEqual(env["LAST_KEY"], "private-example")
        self.assertNotIn("UNRELATED_SECRET", env)
        path.chmod(0o644)
        with self.assertRaises(ValueError):
            policy.credential_environment(config, path)

    def test_path_escape_and_duplicate_chain_rejected(self) -> None:
        self.policy["consumers"]["../escape.json"] = {"primary": "fast,x"}
        self.write_policy()
        with self.assertRaises(ValueError):
            policy.load_policy(self.root)
        self.policy["consumers"].pop("../escape.json")
        self.policy["routes"].append("last,k")
        self.write_policy()
        with self.assertRaises(ValueError):
            policy.load_policy(self.root)


if __name__ == "__main__":
    unittest.main()
