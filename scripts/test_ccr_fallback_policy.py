import copy
import json
import os
import tempfile
import threading
import unittest
import unittest.mock
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
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
                config["Router"]["modelAliases"],
                {},
                "aliases for routes outside the active chain must be pruned",
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

    def test_derive_prunes_dormant_providers_and_aliases(self) -> None:
        config = copy.deepcopy(self.config)
        config["Providers"].append(
            {
                "name": "dormant",
                "models": ["stale"],
                "api_key": "unused",
            }
        )
        config["Router"]["modelAliases"]["stale"] = "dormant,stale"
        derived = policy.derive(config, self.policy, "worker.json")
        provider_names = [provider["name"] for provider in derived["Providers"]]
        self.assertEqual(provider_names, ["fast", "full", "last"])
        self.assertNotIn("stale", derived["Router"]["modelAliases"])

    def test_machine_policy_is_pinned_to_source(self) -> None:
        machine_policy: dict[str, Any] = {
            "version": 1,
            "routes": list(policy.MACHINE_ROUTE_CHAIN),
            "providers": copy.deepcopy(list(policy.MACHINE_SHARED_PROVIDERS.values())),
            "consumers": {
                name: {"primary": primary}
                for name, primary in policy.MACHINE_CONSUMERS.items()
            },
        }
        self.policy = machine_policy
        self.write_policy()
        with unittest.mock.patch.object(policy, "MACHINE_ROOT", self.root):
            policy.load_policy(self.root)

            self.policy["routes"].insert(3, "azure,gpt-6-astra")
            self.write_policy()
            with self.assertRaisesRegex(ValueError, "approved source policy"):
                policy.load_policy(self.root)
            self.policy["routes"].pop(3)

            missing = min(policy.MACHINE_CONSUMERS)
            original_primary = self.policy["consumers"][missing]["primary"]
            self.policy["consumers"].pop(missing)
            self.write_policy()
            with self.assertRaisesRegex(ValueError, "listener registrations"):
                policy.load_policy(self.root)
            self.policy["consumers"][missing] = {"primary": original_primary}

            self.policy["consumers"]["config.json"]["additional_routes"] = [
                "azure,gpt-6-astra"
            ]
            self.write_policy()
            with self.assertRaisesRegex(ValueError, "outside the approved chain"):
                policy.load_policy(self.root)
            self.policy["consumers"]["config.json"]["additional_routes"] = []

            self.policy["providers"][0]["api_base_url"] = "https://tampered.invalid/v1"
            self.write_policy()
            with self.assertRaisesRegex(ValueError, "shared provider definitions"):
                policy.load_policy(self.root)

    def test_minimax_final_tier_uses_the_disambiguated_preview_model(self) -> None:
        self.assertEqual(
            policy.MACHINE_ROUTE_CHAIN[-1],
            "minimax,MiniMax-M3.1-Flash-Preview",
        )
        minimax = policy.MACHINE_SHARED_PROVIDERS["minimax"]
        self.assertEqual(minimax["auth_header"], "authorization")
        self.assertEqual(minimax["models"], ["MiniMax-M3.1-Flash-Preview"])

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


class PreflightTests(unittest.TestCase):
    """Live credential probes use a local fixture, never real providers."""

    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.seen_headers: list[dict[str, str]] = []
        self.status = 401
        handler = self._handler_class()
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        self.addCleanup(self.server.server_close)
        thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(self.server.shutdown)
        self.endpoint = f"http://127.0.0.1:{self.server.server_port}/v1"

    def _handler_class(self) -> type[BaseHTTPRequestHandler]:
        seen = self.seen_headers
        status_ref = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, format: str, *args: Any) -> None:
                pass

            def do_GET(self) -> None:
                seen.append({k.lower(): v for k, v in self.headers.items()})
                self.send_response(status_ref.status)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(b'{"data":[]}')

        return Handler

    def _policy(self, provider: dict[str, Any]) -> None:
        policy.atomic_json(
            self.root / "fallback-policy.json",
            {
                "version": 1,
                "routes": ["fixture,model"],
                "providers": [provider],
                "consumers": {"worker.json": {"primary": "fixture,model"}},
            },
        )
        credentials = self.root / "runtime-credentials.json"
        policy.atomic_json(credentials, {"FIXTURE_KEY": "private-example"})
        credentials.chmod(0o600)

    def test_rejected_credential_fails_preflight_without_leaking_the_secret(
        self,
    ) -> None:
        self._policy(
            {
                "name": "fixture",
                "api_base_url": self.endpoint,
                "api_key": "${FIXTURE_KEY}",
                "protocol": "responses",
                "models": ["model"],
            }
        )
        self.status = 401
        result = policy.preflight(self.root)
        self.assertFalse(result["ok"])
        entry = result["providers"][0]
        self.assertEqual(entry["provider"], "fixture")
        self.assertEqual(entry["detail"], "credential rejected")
        # CCR sends exactly one credential header per protocol, not a retry.
        self.assertEqual([a["status"] for a in entry["attempts"]], [401])
        self.assertNotIn("private-example", json.dumps(result))
        self.assertTrue(self.seen_headers)
        self.assertEqual(
            self.seen_headers[0]["authorization"], "Bearer private-example"
        )

    def test_accepted_credential_passes_and_tries_bearer_before_api_key(self) -> None:
        self._policy(
            {
                "name": "fixture",
                "api_base_url": self.endpoint,
                "api_key": "${FIXTURE_KEY}",
                "protocol": "anthropic",
                "models": ["model"],
            }
        )
        self.status = 200
        result = policy.preflight(self.root)
        self.assertTrue(result["ok"], result)
        entry = result["providers"][0]
        self.assertEqual(entry["status"], 200)
        self.assertEqual(len(self.seen_headers), 1)
        self.assertEqual(self.seen_headers[0]["x-api-key"], "private-example")
        self.assertNotIn("authorization", self.seen_headers[0])

    def test_missing_listing_endpoint_falls_back_to_a_single_token_call(self) -> None:
        """Anthropic upstreams without /models must still preflight."""
        methods: list[str] = []
        bodies: list[dict[str, Any]] = []
        seen = self.seen_headers
        parent = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, format: str, *args: Any) -> None:
                pass

            def _respond(self, code: int) -> None:
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(b"{}")

            def do_GET(self) -> None:
                methods.append("GET")
                seen.append({k.lower(): v for k, v in self.headers.items()})
                self._respond(404)

            def do_POST(self) -> None:
                length = int(self.headers["Content-Length"])
                body = json.loads(self.rfile.read(length))
                bodies.append(body)
                methods.append("POST")
                seen.append({k.lower(): v for k, v in self.headers.items()})
                self._respond(parent.status)

        replacement = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.addCleanup(replacement.server_close)
        threading.Thread(target=replacement.serve_forever, daemon=True).start()
        self.addCleanup(replacement.shutdown)
        self.endpoint = f"http://127.0.0.1:{replacement.server_port}/anthropic/v1"
        self._policy(
            {
                "name": "fixture",
                "api_base_url": self.endpoint,
                "api_key": "${FIXTURE_KEY}",
                "protocol": "anthropic",
                "models": ["flash-model"],
            }
        )
        # Real Anthropic-compatible upstreams such as DeepSeek answer 404 on
        # /models and 200 on /messages; the fixture reproduces exactly that.
        self.status = 200
        result = policy.preflight(self.root)
        self.assertTrue(result["ok"], result)
        self.assertEqual(methods[0], "GET")
        self.assertEqual(methods[-1], "POST")
        self.assertEqual(bodies[-1]["max_tokens"], 1)
        entry = result["providers"][0]
        self.assertEqual(entry["status"], 200)
        self.assertIn("messages", entry["detail"])
        self.assertNotIn("private-example", json.dumps(result))

    def test_unresolved_credential_is_reported_before_any_network_call(self) -> None:
        policy.atomic_json(
            self.root / "fallback-policy.json",
            {
                "version": 1,
                "routes": ["fixture,model"],
                "providers": [
                    {
                        "name": "fixture",
                        "api_base_url": self.endpoint,
                        "api_key": "${MISSING_KEY}",
                        "protocol": "responses",
                        "models": ["model"],
                    }
                ],
                "consumers": {"worker.json": {"primary": "fixture,model"}},
            },
        )
        credentials = self.root / "runtime-credentials.json"
        policy.atomic_json(credentials, {})
        credentials.chmod(0o600)
        with self.assertRaises(ValueError):
            policy.preflight(self.root)
        self.assertEqual(self.seen_headers, [])
