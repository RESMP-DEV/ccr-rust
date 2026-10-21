import copy
import hashlib
import json
import os
import sys
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

    def test_sync_takes_pre_write_recovery_snapshot_only_when_writing(self) -> None:
        with (
            unittest.mock.patch("ccr_recovery.pre_snapshot") as pre,
            unittest.mock.patch.object(
                policy, "should_recovery_snapshot", lambda _: True
            ),
        ):
            policy.sync(self.root)
        self.assertTrue(pre.called)
        self.assertEqual(pre.call_args.kwargs.get("label"), "pre-sync")
        pre.reset_mock()
        policy.sync(self.root, check=True)
        self.assertFalse(pre.called)

    def test_missing_recovery_module_does_not_block_sync(self) -> None:
        with (
            unittest.mock.patch.dict(sys.modules, {"ccr_recovery": None}),
            unittest.mock.patch.object(
                policy, "should_recovery_snapshot", lambda _: True
            ),
            unittest.mock.patch("sys.stderr") as stderr,
        ):
            result = policy.sync(self.root)
        self.assertTrue(result["changed"])
        output = "".join(call.args[0] for call in stderr.write.call_args_list)
        self.assertIn("pre-write snapshot failed to launch", output)

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
        for name, settings in self.policy["consumers"].items():
            config = policy.read_json(self.root / name)
            self.assertEqual(config["Router"]["retrySweeps"]["maxHoldMs"], 12345)
            self.assertEqual(
                config["Router"]["modelAliases"],
                {
                    "x": "fast,x",
                    "y": "fast,y",
                    "z": "full,z",
                    "k": "last,k",
                    "ccr-primary": settings["primary"],
                },
                "aliases outside the active chain are pruned, every"
                " unambiguous tier id is derived as an alias so advertised"
                " bare ids stay routable, and ccr-primary tracks each"
                " consumer's primary tier",
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
        self.assertEqual(
            [provider["models"] for provider in derived["Providers"]],
            [["x", "y"], ["z"], ["k"]],
        )
        self.assertNotIn("stale", derived["Router"]["modelAliases"])

    def test_bare_tier_ids_get_derived_aliases(self) -> None:
        # An operator alias is authoritative: injection never overrides it,
        # even when it points a tier's bare id at a different chain tier.
        config = copy.deepcopy(self.config)
        config["Router"]["modelAliases"]["y"] = "fast,x"
        derived = policy.derive(config, self.policy, "worker.json")
        self.assertEqual(
            derived["Router"]["modelAliases"],
            {
                "y": "fast,x",
                "x": "fast,x",
                "z": "full,z",
                "k": "last,k",
                "ccr-primary": "fast,x",
            },
        )
        # A model name offered by two providers is ambiguous: no alias is
        # derived for it rather than silently choosing one provider.
        self.policy["routes"].insert(3, "full,y")
        ambiguous = copy.deepcopy(self.config)
        ambiguous["Providers"][1]["models"].append("y")
        derived = policy.derive(ambiguous, self.policy, "worker.json")
        self.assertNotIn("y", derived["Router"]["modelAliases"])
        self.assertEqual(derived["Router"]["modelAliases"]["x"], "fast,x")

    def test_machine_policy_is_pinned_to_source(self) -> None:
        machine_policy: dict[str, Any] = {
            "version": 1,
            "routes": list(policy.MACHINE_ROUTE_CHAIN),
            "providers": copy.deepcopy(list(policy.MACHINE_SHARED_PROVIDERS.values())),
            "consumers": {
                name: {
                    "primary": primary,
                    "additional_routes": list(
                        policy.MACHINE_CONSUMER_ADDITIONAL_ROUTES.get(name, ())
                    ),
                }
                for name, primary in policy.MACHINE_CONSUMERS.items()
            },
        }
        expected_pin = hashlib.sha256(
            json.dumps(machine_policy, sort_keys=True).encode()
        ).hexdigest()
        self.assertEqual(expected_pin, policy.MACHINE_POLICY_INTEGRITY_SHA256)
        self.policy = machine_policy
        self.write_policy()
        with unittest.mock.patch.object(policy, "MACHINE_ROOT", self.root):
            policy.load_policy(self.root)

            self.policy["routes"].append("azure,gpt-6-astra")
            self.write_policy()
            with self.assertRaisesRegex(ValueError, "approved source policy"):
                policy.load_policy(self.root)
            self.policy["routes"].pop()

            missing = min(policy.MACHINE_CONSUMERS)
            original_settings = copy.deepcopy(self.policy["consumers"][missing])
            self.policy["consumers"].pop(missing)
            self.write_policy()
            with self.assertRaisesRegex(ValueError, "listener registrations"):
                policy.load_policy(self.root)
            self.policy["consumers"][missing] = original_settings

            original_extras = self.policy["consumers"]["config.json"][
                "additional_routes"
            ]
            self.policy["consumers"]["config.json"]["additional_routes"] = [
                "azure,gpt-6-astra"
            ]
            self.write_policy()
            with self.assertRaisesRegex(ValueError, "additional routes must remain"):
                policy.load_policy(self.root)
            self.policy["consumers"]["config.json"]["additional_routes"] = (
                original_extras
            )

            self.policy["providers"][0]["api_base_url"] = "https://tampered.invalid/v1"
            self.write_policy()
            with self.assertRaisesRegex(ValueError, "shared provider definitions"):
                policy.load_policy(self.root)

    def test_machine_policy_rejects_unconstrained_whole_body_edits(self) -> None:
        self.policy = {
            "version": 1,
            "routes": list(policy.MACHINE_ROUTE_CHAIN),
            "providers": copy.deepcopy(list(policy.MACHINE_SHARED_PROVIDERS.values())),
            "consumers": {
                name: {
                    "primary": primary,
                    "additional_routes": list(
                        policy.MACHINE_CONSUMER_ADDITIONAL_ROUTES.get(name, ())
                    ),
                }
                for name, primary in policy.MACHINE_CONSUMERS.items()
            },
        }
        self.write_policy()
        with unittest.mock.patch.object(policy, "MACHINE_ROOT", self.root):
            policy.load_policy(self.root)
            self.policy["unconstrained"] = {"route": "unapproved,model"}
            self.write_policy()
            with self.assertRaisesRegex(ValueError, "approved integrity pin"):
                policy.load_policy(self.root)

    def test_minimax_worker_primary_uses_the_official_non_preview_model(
        self,
    ) -> None:
        # 2026-10-21: MiniMax is the scoped-worker primary, so its reviewed ID
        # leads the shared chain rather than sitting at the final fallback.
        self.assertEqual(
            policy.MACHINE_ROUTE_CHAIN[0],
            "minimax,MiniMax-M3.1-Flash",
        )
        minimax = policy.MACHINE_SHARED_PROVIDERS["minimax"]
        self.assertEqual(minimax["auth_header"], "authorization")
        self.assertEqual(minimax["models"], ["MiniMax-M3.1-Flash"])
        self.assertEqual(minimax["transformer"], {"use": ["minimax"]})

    def test_probe_extra_headers_ride_with_the_credential(self) -> None:
        gmi = policy.MACHINE_SHARED_PROVIDERS["gmi"]
        requests = policy._probe_requests(gmi, "secret")
        self.assertEqual([kind for kind, _ in requests], ["models"])
        headers = {key.lower(): value for key, value in requests[0][1].header_items()}
        self.assertEqual(headers["authorization"], "Bearer secret")
        self.assertEqual(headers["user-agent"], "ccr-fallback-policy-preflight/1")

    def test_removed_tiers_stay_registered_providers(self) -> None:
        # 2026-10-11: gmi (unreliable) and deepseek (balance exhausted) left
        # the chain but stay registered so re-adding a chain entry restores
        # them without rewiring credentials. Qwen joined that group on
        # 2026-10-19 when its monthly window ran out.
        self.assertEqual(
            policy.MACHINE_ROUTE_CHAIN,
            (
                "minimax,MiniMax-M3.1-Flash",
                "zai,glm-5.3-flash",
                "deepseek,deepseek-flash",
                "strata-b550,qwen3.8-flash-next-coder",
                "local27,qwen3.8-27b-mxfp4",
            ),
        )
        self.assertNotIn("gmi,Qwen/Qwen3.8-Max-0902", policy.MACHINE_ROUTE_CHAIN)
        self.assertIn("deepseek,deepseek-flash", policy.MACHINE_ROUTE_CHAIN)
        self.assertNotIn("qwen,qwen3.8-flash", policy.MACHINE_ROUTE_CHAIN)
        self.assertNotIn("qwen,deepseek-v4.1-flash", policy.MACHINE_ROUTE_CHAIN)
        self.assertIn("gmi", policy.MACHINE_SHARED_PROVIDERS)
        self.assertIn("deepseek", policy.MACHINE_SHARED_PROVIDERS)
        self.assertIn("qwen", policy.MACHINE_SHARED_PROVIDERS)
        self.assertEqual(
            policy.MACHINE_SHARED_PROVIDERS["gmi"]["api_base_url"],
            "https://api.gmi-serving.com/v1",
        )
        self.assertIn(
            "User-Agent", policy.MACHINE_SHARED_PROVIDERS["gmi"]["extra_headers"]
        )

    def test_local_fallback_order_and_main_auth_primary_unchanged(
        self,
    ) -> None:
        # 2026-10-21: GLM Flash moved to the final shared fallback so the
        # exhausted GLM utilization window is only spent after MiniMax. Strata
        # B550 is the stronger local coding tier, while local27 remains final.
        self.assertNotIn("zai,glm-5.3-flashx", policy.MACHINE_ROUTE_CHAIN)
        self.assertEqual(
            policy.MACHINE_ROUTE_CHAIN[-4],
            "zai,glm-5.3-flash",
        )
        # 2026-10-21: the resident local MLX model is the last crawl-back tier.
        self.assertEqual(
            policy.MACHINE_ROUTE_CHAIN[-1],
            "local27,qwen3.8-27b-mxfp4",
        )
        self.assertEqual(
            policy.MACHINE_CONSUMERS["glm-workers.json"],
            "minimax,MiniMax-M3.1-Flash",
        )
        self.assertEqual(policy.MACHINE_CONSUMERS["config.json"], "zai,glm-5.3")

    def test_worker_chain_leads_with_minimax_and_keeps_glm_flash_fallback(
        self,
    ) -> None:
        # 2026-10-21: the scoped-worker primary moved to MiniMax because the
        # Z.AI GLM plan keeps exhausting its monthly utilization window; GLM
        # Flash remains behind MiniMax; both local tiers follow the cloud lane.
        self.assertEqual(
            policy.MACHINE_ROUTE_CHAIN,
            (
                "minimax,MiniMax-M3.1-Flash",
                "zai,glm-5.3-flash",
                "deepseek,deepseek-flash",
                "strata-b550,qwen3.8-flash-next-coder",
                "local27,qwen3.8-27b-mxfp4",
            ),
        )
        self.assertNotIn("zai,glm-5.3", policy.MACHINE_ROUTE_CHAIN)
        self.assertEqual(policy.MACHINE_CONSUMERS["config.json"], "zai,glm-5.3")
        self.assertEqual(
            policy.MACHINE_CONSUMERS["glm-workers.json"],
            "minimax,MiniMax-M3.1-Flash",
        )

    def test_main_and_auth_share_the_chain_after_the_qwen_removal(self) -> None:
        machine_policy = {
            "version": 1,
            "routes": list(policy.MACHINE_ROUTE_CHAIN),
            "providers": copy.deepcopy(list(policy.MACHINE_SHARED_PROVIDERS.values())),
            "consumers": {
                name: {
                    "primary": primary,
                    "additional_routes": list(extras),
                }
                for name, primary in policy.MACHINE_CONSUMERS.items()
                for extras in (policy.MACHINE_CONSUMER_ADDITIONAL_ROUTES.get(name, ()),)
            },
        }
        config: dict[str, Any] = {
            "Providers": [],
            "Router": {
                "default": policy.MACHINE_CONSUMERS["config.json"],
                "retrySweeps": {"enabled": True, "maxHoldMs": 60_000},
            },
        }
        derived = policy.derive(config, machine_policy, "config.json")
        # The private Qwen Max seat left with the rest of the lane, so main and
        # auth now run the plain shared chain behind their GLM-5.3 primary.
        self.assertEqual(
            derived["Router"]["tiers"],
            [
                "zai,glm-5.3",
                "minimax,MiniMax-M3.1-Flash",
                "zai,glm-5.3-flash",
                "deepseek,deepseek-flash",
                "strata-b550,qwen3.8-flash-next-coder",
                "local27,qwen3.8-27b-mxfp4",
            ],
        )
        self.assertEqual(policy.MACHINE_CONSUMER_ADDITIONAL_ROUTES, {})

    def test_local27_is_streaming_backend_bounded_like_its_proxy(self) -> None:
        local = policy.MACHINE_SHARED_PROVIDERS["local27"]
        # :8080 is the admission proxy and rejects stream:true; :8081 streams.
        self.assertEqual(local["api_base_url"], "http://127.0.0.1:8081/v1")
        self.assertEqual(local["protocol"], "openai")
        self.assertEqual(local["maxInflight"], 4)

    def test_strata_b550_is_keyed_single_flight_and_nonthinking(self) -> None:
        strata = policy.MACHINE_SHARED_PROVIDERS["strata-b550"]
        self.assertEqual(strata["api_base_url"], "http://127.0.0.1:18081/v1")
        self.assertEqual(strata["api_key"], "${CCR_STRATA_B550_API_KEY}")
        self.assertEqual(strata["protocol"], "openai")
        self.assertEqual(strata["maxInflight"], 1)
        self.assertEqual(strata["force_reasoning_effort"], "none")
        self.assertEqual(
            strata["transformer"],
            {"use": [["maxtoken", {"max_tokens": 4096, "only_if_missing": True}]]},
        )

    def test_qwen_token_plan_uses_verified_anthropic_endpoint(self) -> None:
        qwen = policy.MACHINE_SHARED_PROVIDERS["qwen"]
        self.assertEqual(
            qwen["api_base_url"],
            "https://token-plan.maas.qwencloudapi.com/apps/anthropic/v1",
        )
        self.assertEqual(qwen["api_key"], "${CCR_QWEN_API_KEY}")
        self.assertEqual(qwen["protocol"], "anthropic")
        self.assertEqual(
            qwen["transformer"],
            {"use": [["maxtoken", {"max_tokens": 32768, "only_if_missing": True}]]},
        )
        self.assertIn("qwen3.8-flash", qwen["models"])
        self.assertIn("deepseek-v4.1-flash-1017", qwen["models"])
        self.assertIn("deepseek-v4.1-flash", qwen["models"])

    def test_qwen_stays_registered_but_unrouted_after_exhaustion(self) -> None:
        # The token plan kept its provider and credential so a dated
        # re-approval restores the lane, but no route references it.
        self.assertIn("qwen", policy.MACHINE_SHARED_PROVIDERS)
        self.assertEqual(
            [
                route
                for route in policy.MACHINE_ROUTE_CHAIN
                if route.startswith("qwen,")
            ],
            [],
        )
        self.assertEqual(policy.MACHINE_CONSUMER_ADDITIONAL_ROUTES, {})

    def test_zai_provider_registered_for_restored_routes(self) -> None:
        zai = policy.MACHINE_SHARED_PROVIDERS["zai"]
        self.assertEqual(zai["api_base_url"], "https://api.z.ai/api/v1")
        self.assertEqual(zai["api_key"], "${CCR_ZAI_API_KEY}")
        self.assertEqual(zai["protocol"], "responses")
        self.assertIn("glm-5.3", zai["models"])
        self.assertIn("glm-5.3-flash", zai["models"])

    def test_governed_consumers_cannot_preserve_an_infinite_hold(self) -> None:
        config = copy.deepcopy(self.config)
        config["Router"]["retrySweeps"] = {
            "enabled": True,
            "maxSweeps": 0,
            "sweepCooldownMs": 100,
            "maxHoldMs": 0,
        }
        derived = policy.derive(config, self.policy, "worker.json")
        self.assertEqual(derived["Router"]["retrySweeps"]["maxHoldMs"], 60_000)

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
            unittest.mock.patch.object(
                policy, "should_recovery_snapshot", lambda _: True
            ),
            unittest.mock.patch("ccr_recovery.pre_snapshot") as pre_snapshot,
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
        self.assertTrue(pre_snapshot.called)
        self.assertEqual(pre_snapshot.call_args.kwargs.get("label"), "pre-serve")
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

    def test_registered_not_routed_provider_is_skipped_not_probed(self) -> None:
        """A registered provider no route references must not gate preflight."""
        self._policy(
            {
                "name": "fixture",
                "api_base_url": self.endpoint,
                "api_key": "${FIXTURE_KEY}",
                "protocol": "responses",
                "models": ["model"],
            }
        )
        dormant_policy = json.loads(
            (self.root / "fallback-policy.json").read_text(encoding="utf-8")
        )
        # Port 9 (discard) would fail as a transport error if ever probed.
        dormant_policy["providers"].append(
            {
                "name": "dormant",
                "api_base_url": "http://127.0.0.1:9/v1",
                "api_key": "${FIXTURE_KEY}",
                "protocol": "responses",
                "models": ["stale"],
            }
        )
        policy.atomic_json(self.root / "fallback-policy.json", dormant_policy)
        self.status = 200
        result = policy.preflight(self.root)
        self.assertTrue(result["ok"], result)
        self.assertEqual(result["registered_not_routed"], ["dormant"])
        self.assertEqual(
            [entry["provider"] for entry in result["providers"]], ["fixture"]
        )

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
