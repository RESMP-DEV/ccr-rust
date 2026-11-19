#!/usr/bin/env python3
"""Tests for CCR worker usage summaries."""

from __future__ import annotations

import io
import json
import unittest
from contextlib import redirect_stdout
from datetime import date, datetime, timedelta
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

import ccr_worker_usage as usage


class WorkerUsageTests(unittest.TestCase):
    def test_parse_listener_metrics_reads_active_and_tier_counters(self) -> None:
        metrics = """# HELP ccr_active_requests Current
ccr_active_requests 2
ccr_requests_total{tier="zai"} 28
ccr_requests_total{tier="minimax"} 1
ccr_requests_total{tier="other"} 1e+06
ccr_requests_total{tier="zai"} 4
"""
        result = usage.parse_listener_metrics(metrics, "http://127.0.0.1:3457/metrics")
        self.assertEqual(result.active_requests, 2)
        self.assertEqual(
            result.tier_requests, {"minimax": 1, "other": 1000000, "zai": 4}
        )

    def test_nonloopback_listener_is_rejected(self) -> None:
        for url in (
            "https://127.0.0.1/metrics",
            "http://example.com@127.0.0.1/metrics",
        ):
            with self.assertRaises(ValueError):
                usage.validate_local_http_url(url)

    def test_listener_hostname_must_resolve_only_to_loopback(self) -> None:
        with (
            patch.object(
                usage.socket,
                "getaddrinfo",
                return_value=[(None, None, None, None, ("10.0.0.1", 0))],
            ),
            self.assertRaisesRegex(ValueError, "loopback"),
        ):
            usage.validate_local_http_url("http://localhost/metrics")

    def test_metrics_opener_never_follows_redirects(self) -> None:
        handler = usage.NoRedirectHandler()
        self.assertIsNone(
            handler.redirect_request(
                req=None,
                fp=None,
                code=302,
                msg="Found",
                headers=None,
                newurl="http://example.test/metrics",
            )
        )

        with patch.object(
            usage.socket,
            "getaddrinfo",
            return_value=[(None, None, None, None, ("::1", 0))],
        ):
            usage.validate_local_http_url("http://localhost/metrics")

    def test_run_summary_counts_only_requested_local_dates(self) -> None:
        with TemporaryDirectory() as directory:
            root = Path(directory)
            records = [
                (
                    (date.today() - timedelta(days=2)).strftime("%Y%m%d")
                    + "-010101-old",
                    {"outcome": "completed"},
                ),
                (
                    date.today().strftime("%Y%m%d") + "-010101-one",
                    {
                        "outcome": "completed",
                        "timeout_profile": "review",
                        "requested_route": "zai,glm-5.3-flash",
                        "read_only": True,
                        "network_allowed": False,
                        "elapsed_seconds": 12.5,
                    },
                ),
                (
                    date.today().strftime("%Y%m%d") + "-020202-two",
                    {
                        "outcome": "timed_out",
                        "timeout_profile": "review",
                        "requested_route": "zai,glm-5.3-flash",
                        "read_only": True,
                        "network_allowed": True,
                        "elapsed_seconds": 2.5,
                    },
                ),
                (
                    date.today().strftime("%Y%m%d") + "-030303-bad",
                    ["not", "an", "object"],
                ),
                (
                    date.today().strftime("%Y%m%d") + "-040304-bad-elapsed",
                    {"outcome": "completed", "elapsed_seconds": "not-a-number"},
                ),
                (
                    "99999999-050305-invalid-date",
                    {"outcome": "completed"},
                ),
            ]
            for name, status in records:
                run_dir = root / name
                run_dir.mkdir()
                (run_dir / "status.json").write_text(
                    json.dumps(status), encoding="utf-8"
                )
            orphan = root / (date.today().strftime("%Y%m%d") + "-060306-orphan")
            orphan.mkdir()
            (orphan / "events.jsonl").write_text("", encoding="utf-8")

            result = usage.summarize_runs(root, days=1)

        self.assertEqual(result.runs, 5)
        self.assertEqual(
            result.outcomes,
            {
                "completed": 1,
                "receipt_pending": 1,
                "timed_out": 1,
                "unreadable": 2,
            },
        )
        self.assertEqual(result.timeout_profiles, {"review": 2})
        self.assertEqual(result.requested_routes, {"zai,glm-5.3-flash": 2})
        self.assertEqual((result.read_only, result.network_allowed), (2, 1))
        self.assertEqual(result.elapsed_seconds_total, 15.0)

    def test_run_summary_aggregates_qoder_lane_outcomes(self) -> None:
        with TemporaryDirectory() as directory:
            root = Path(directory)
            today = date.today().strftime("%Y%m%d")
            statuses = [
                (
                    "fallback-attempt",
                    {
                        "outcome": "completed",
                        "engine": "codex-qoder-fallback",
                        "qoder_primary_attempt": {"outcome": "completed"},
                    },
                ),
                (
                    "attempted-precedence",
                    {
                        "outcome": "completed",
                        "engine": "qoder",
                        "served_route": "qoder,qmodel_latest",
                        "qoder_primary_attempt": {"outcome": "completed"},
                        "qoder_bypass_reason": "network",
                    },
                ),
                (
                    "bypass-network",
                    {"outcome": "completed", "qoder_bypass_reason": "network"},
                ),
                (
                    "bypass-disabled",
                    {"outcome": "completed", "qoder_bypass_reason": "disabled"},
                ),
                ("not-recorded", {"outcome": "completed"}),
                ("unreadable", ["not", "an", "object"]),
            ]
            for index, (name, status) in enumerate(statuses):
                run_dir = root / f"{today}-{index:06d}-{name}"
                run_dir.mkdir()
                (run_dir / "status.json").write_text(
                    json.dumps(status), encoding="utf-8"
                )
            (root / f"{today}-900000-orphan").mkdir()

            result = usage.summarize_runs(root, days=1)

        self.assertEqual(
            result.qoder_lane,
            {
                "attempted": 2,
                "disabled": 1,
                "fell_back": 1,
                "network": 1,
                "not_recorded": 1,
                "outcome:completed": 2,
                "served": 1,
            },
        )

    def test_run_summary_classifies_real_qoder_receipts(self) -> None:
        with TemporaryDirectory() as directory:
            root = Path(directory)
            today = date.today().strftime("%Y%m%d")
            statuses = [
                (
                    "served-engine",
                    {
                        "outcome": "completed",
                        "engine": "qoder",
                        "requested_route": "qoder,Qwen4-Max-Preview",
                        "served_route": "qoder,qmodel_latest",
                        "qoder_primary_requested": True,
                        "qoder_primary_attempt": {"outcome": "completed"},
                    },
                ),
                (
                    "served-route",
                    {
                        "outcome": "completed",
                        "requested_route": "qoder,Qwen4-Max-Preview",
                        "served_route": "qoder,qmodel_latest",
                        "qoder_primary_requested": True,
                        "qoder_primary_attempt": {"outcome": "completed"},
                    },
                ),
                (
                    "fallback-write-failure",
                    {
                        "outcome": "completed",
                        "engine": "codex-qoder-fallback",
                        "requested_route": "qoder,Qwen4-Max-Preview",
                        "qoder_primary_requested": True,
                        "qoder_primary_attempt": {"outcome": "completed"},
                    },
                ),
                (
                    "fallback-failed",
                    {
                        "outcome": "completed",
                        "engine": "codex-qoder-fallback",
                        "requested_route": "qoder,Qwen4-Max-Preview",
                        "qoder_primary_requested": True,
                        "qoder_primary_attempt": {"outcome": "failed"},
                    },
                ),
                (
                    "fallback-rate-limited",
                    {
                        "outcome": "completed",
                        "engine": "codex-qoder-fallback",
                        "requested_route": "qoder,Qwen4-Max-Preview",
                        "qoder_primary_requested": True,
                        "qoder_primary_attempt": {"outcome": "rate_limited"},
                    },
                ),
                (
                    "running",
                    {
                        "outcome": "running",
                        "requested_route": "qoder,Qwen4-Max-Preview",
                        "qoder_primary_requested": True,
                    },
                ),
                (
                    "served-without-attempt",
                    {
                        "outcome": "completed",
                        "engine": "qoder",
                        "served_route": "qoder,qmodel_latest",
                    },
                ),
                (
                    "eligible-flag",
                    {
                        "outcome": "completed",
                        "qoder_primary_requested": True,
                        "requested_route": "qoder,Qwen4-Max-Preview",
                    },
                ),
                (
                    "eligible-route",
                    {
                        "outcome": "completed",
                        "requested_route": "qoder,Qwen4-Max-Preview",
                    },
                ),
                (
                    "opt-out",
                    {
                        "outcome": "completed",
                        "qoder_bypass_reason": "disabled",
                        "requested_route": "minimax,MiniMax-M3.1-Flash",
                    },
                ),
                ("legacy", {"outcome": "completed"}),
            ]
            for index, (name, status) in enumerate(statuses):
                run_dir = root / f"{today}-{index:06d}-{name}"
                run_dir.mkdir()
                (run_dir / "status.json").write_text(
                    json.dumps(status), encoding="utf-8"
                )

            result = usage.summarize_runs(root, days=1)

        self.assertEqual(
            result.qoder_lane,
            {
                "attempted": 5,
                "disabled": 1,
                "eligible_not_attempted": 2,
                "fell_back": 3,
                "not_recorded": 3,
                "outcome:completed": 3,
                "outcome:failed": 1,
                "outcome:rate_limited": 1,
                "served": 2,
            },
        )

    @staticmethod
    def write_rollout(root: Path, relative: str, records: list[object]) -> Path:
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            "\n".join(
                record if isinstance(record, str) else json.dumps(record)
                for record in records
            )
            + "\n",
            encoding="utf-8",
        )
        return path

    def test_parent_session_summary_counts_codex_tui_delegation(self) -> None:
        now = (
            datetime.now()
            .astimezone()
            .replace(hour=12, minute=0, second=0, microsecond=0)
        )
        stamp = now.isoformat()
        old_stamp = (now - timedelta(days=3)).isoformat()
        day_path = now.strftime("%Y/%m/%d")

        def meta(originator: str, cwd: str, when: str) -> dict[str, object]:
            return {
                "timestamp": when,
                "type": "session_meta",
                "payload": {
                    "timestamp": when,
                    "cwd": cwd,
                    "originator": originator,
                },
            }

        def response(payload: dict[str, object]) -> dict[str, object]:
            return {"timestamp": stamp, "type": "response_item", "payload": payload}

        def shell_call(command: str, call_id: str) -> dict[str, object]:
            return response(
                {
                    "type": "function_call",
                    "name": "exec_command",
                    "arguments": json.dumps({"cmd": command}),
                    "call_id": call_id,
                }
            )

        ledger = "Workers: audit\nParent-only: acceptance\nTerminal budget: two"
        with TemporaryDirectory() as directory:
            root = Path(directory)
            self.write_rollout(
                root,
                f"{day_path}/rollout-a.jsonl",
                [
                    meta("codex-tui", "/repo/a", stamp),
                    {
                        "timestamp": stamp,
                        "type": "event_msg",
                        "payload": {
                            "type": "task_started",
                            "turn_attribution": {"turn_trigger": "user"},
                        },
                    },
                    response(
                        {
                            "type": "message",
                            "role": "user",
                            "content": [
                                {
                                    "type": "input_text",
                                    "text": "PROMPT_TEXT_MUST_NOT_LEAK",
                                }
                            ],
                        }
                    ),
                    {
                        "timestamp": stamp,
                        "type": "turn_context",
                        "payload": {"cwd": "/repo/a"},
                    },
                    {
                        "timestamp": stamp,
                        "type": "turn_context",
                        "payload": {"cwd": "/repo/a"},
                    },
                    shell_call(
                        "codex-ccr-worker --cwd /repo/a --read-only 'brief'",
                        "call-1",
                    ),
                    response(
                        {
                            "type": "function_call",
                            "name": "create_thread",
                            "arguments": json.dumps({"prompt": "child task"}),
                            "call_id": "call-2",
                        }
                    ),
                    shell_call(
                        "CCR_DISABLE_QODER_WORKER=1 codex-ccr-worker "
                        "--no-qoder-primary --cwd /repo/a 'brief'",
                        "call-3",
                    ),
                    response(
                        {
                            "type": "function_call",
                            "name": "shell",
                            "arguments": json.dumps(
                                {"command": ["bash", "-lc", "rg ledger docs"]}
                            ),
                            "call_id": "call-4",
                        }
                    ),
                    response(
                        {
                            "type": "function_call",
                            "name": "apply_patch",
                            "arguments": "*** Begin Patch",
                            "call_id": "call-5",
                        }
                    ),
                    response(
                        {
                            "type": "message",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": ledger}],
                        }
                    ),
                    {
                        "timestamp": stamp,
                        "type": "event_msg",
                        "payload": {"type": "agent_message", "message": ledger},
                    },
                    "{not-json",
                ],
            )
            self.write_rollout(
                root,
                f"{day_path}/rollout-b.jsonl",
                [
                    meta("codex-tui", "/repo/b", stamp),
                    {
                        "timestamp": stamp,
                        "type": "turn_context",
                        "payload": {"cwd": "/repo/b"},
                    },
                    shell_call("rg ledger docs", "call-6"),
                    response(
                        {
                            "type": "message",
                            "role": "assistant",
                            "content": [
                                {
                                    "type": "output_text",
                                    "text": "Workers: partial only",
                                }
                            ],
                        }
                    ),
                ],
            )
            self.write_rollout(
                root,
                f"{day_path}/rollout-c.jsonl",
                [
                    meta("codex_cli_rs", "/repo/c", stamp),
                    {
                        "timestamp": stamp,
                        "type": "turn_context",
                        "payload": {"cwd": "/repo/c"},
                    },
                ],
            )
            self.write_rollout(
                root,
                f"{day_path}/rollout-old.jsonl",
                [
                    meta("codex-tui", "/repo/old", old_stamp),
                    {
                        "timestamp": old_stamp,
                        "type": "turn_context",
                        "payload": {"cwd": "/repo/old"},
                    },
                ],
            )
            (root / day_path / "notes.txt").write_text("ignored", encoding="utf-8")

            result = usage.summarize_parent_sessions(root, days=1)

        self.assertEqual(result.sessions, 2)
        self.assertEqual(result.root_parent_sessions, 1)
        self.assertEqual(result.native_child_sessions, 1)
        self.assertEqual(result.sessions_with_native_child_delegation, 1)
        self.assertEqual(result.sessions_with_no_worker_launch, 0)
        self.assertEqual(result.turns, 3)
        self.assertEqual(result.tool_calls, 6)
        self.assertEqual(result.shell_calls, 4)
        self.assertEqual(result.native_child_tool_calls, 1)
        self.assertEqual(result.codex_ccr_worker_launches, 2)
        self.assertEqual(result.create_thread_launches, 1)
        self.assertEqual(result.ledger_messages, 1)
        self.assertEqual(result.malformed_lines, 1)
        self.assertEqual(
            [record.session for record in result.items],
            [f"{day_path}/rollout-a.jsonl", f"{day_path}/rollout-b.jsonl"],
        )
        first, second = result.items
        self.assertEqual(first.cwd, "/repo/a")
        self.assertEqual((first.turns, first.tool_calls, first.shell_calls), (2, 5, 3))
        self.assertEqual(first.codex_ccr_worker_launches, 2)
        self.assertEqual(first.create_thread_launches, 1)
        self.assertEqual(first.ledger_messages, 1)
        self.assertFalse(first.native_child)
        self.assertEqual(second.cwd, "/repo/b")
        self.assertEqual(second.codex_ccr_worker_launches, 0)
        self.assertTrue(second.native_child)

        json_output = io.StringIO()
        with redirect_stdout(json_output):
            usage.print_usage(None, None, json_output=True, parent_sessions=result)
        payload = json.loads(json_output.getvalue())["parent_sessions"]
        self.assertEqual(payload["sessions"], 2)
        self.assertEqual(payload["root_parent_sessions"], 1)
        self.assertEqual(payload["native_child_sessions"], 1)
        self.assertEqual(payload["sessions_with_native_child_delegation"], 1)
        self.assertEqual(payload["sessions_with_no_worker_launch"], 0)
        self.assertEqual(payload["native_child_tool_calls"], 1)
        self.assertEqual(
            [item["native_child"] for item in payload["items"]], [False, True]
        )
        self.assertNotIn("PROMPT_TEXT_MUST_NOT_LEAK", json_output.getvalue())

        text_output = io.StringIO()
        with redirect_stdout(text_output):
            usage.print_usage(None, None, json_output=False, parent_sessions=result)
        rendered = text_output.getvalue()
        self.assertIn("parent_sessions: last 1 day(s), sessions=2", rendered)
        self.assertIn("root_parent_sessions=1", rendered)
        self.assertIn("native_child_sessions=1", rendered)
        self.assertIn("sessions_with_native_child_delegation=1", rendered)
        self.assertIn("sessions_with_no_worker_launch=0", rendered)
        self.assertIn("native_child_tool_calls=1", rendered)
        self.assertIn("native_child=False", rendered)
        self.assertIn("native_child=True", rendered)
        self.assertNotIn("PROMPT_TEXT_MUST_NOT_LEAK", rendered)

    def test_parent_session_summary_classifies_first_turn_trigger(self) -> None:
        now = (
            datetime.now()
            .astimezone()
            .replace(hour=12, minute=0, second=0, microsecond=0)
        )
        stamp = now.isoformat()
        day_path = now.strftime("%Y/%m/%d")

        def meta(cwd: str) -> dict[str, object]:
            return {
                "timestamp": stamp,
                "type": "session_meta",
                "payload": {
                    "timestamp": stamp,
                    "cwd": cwd,
                    "originator": "codex-tui",
                },
            }

        def task_started(trigger: object) -> dict[str, object]:
            return {
                "timestamp": stamp,
                "type": "event_msg",
                "payload": {
                    "type": "task_started",
                    "turn_attribution": {"turn_trigger": trigger},
                },
            }

        def shell_call(command: str, call_id: str) -> dict[str, object]:
            return {
                "timestamp": stamp,
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "exec_command",
                    "arguments": json.dumps({"cmd": command}),
                    "call_id": call_id,
                },
            }

        with TemporaryDirectory() as directory:
            root = Path(directory)
            self.write_rollout(
                root,
                f"{day_path}/rollout-a-root.jsonl",
                [
                    meta("/repo/root"),
                    task_started("user"),
                    task_started(None),
                    {
                        "timestamp": stamp,
                        "type": "turn_context",
                        "payload": {"cwd": "/repo/root"},
                    },
                    shell_call("rg ledger docs", "call-root"),
                ],
            )
            self.write_rollout(
                root,
                f"{day_path}/rollout-b-child.jsonl",
                [
                    meta("/repo/child"),
                    task_started(None),
                    task_started("user"),
                    {
                        "timestamp": stamp,
                        "type": "turn_context",
                        "payload": {"cwd": "/repo/child"},
                    },
                    shell_call("rg child docs", "call-child-1"),
                    shell_call("rg child tests", "call-child-2"),
                ],
            )

            result = usage.summarize_parent_sessions(root, days=1)

        self.assertEqual(result.sessions, 2)
        self.assertEqual(result.root_parent_sessions, 1)
        self.assertEqual(result.native_child_sessions, 1)
        self.assertEqual(result.sessions_with_no_worker_launch, 1)
        self.assertEqual(result.native_child_tool_calls, 2)
        self.assertEqual(result.tool_calls, 3)
        root_item, child_item = result.items
        self.assertFalse(root_item.native_child)
        self.assertTrue(child_item.native_child)
        self.assertEqual(root_item.codex_ccr_worker_launches, 0)
        self.assertEqual(child_item.codex_ccr_worker_launches, 0)

    def test_missing_sessions_root_is_rejected(self) -> None:
        with (
            TemporaryDirectory() as directory,
            self.assertRaisesRegex(ValueError, "--sessions-root must be an existing"),
        ):
            usage.summarize_parent_sessions(Path(directory) / "missing", days=1)

    def test_parent_sessions_argument_rules(self) -> None:
        args = usage.parse_args(
            [
                "--parent-sessions",
                "--sessions-root",
                "codex-sessions",
                "--days",
                "3",
            ]
        )
        self.assertTrue(args.parent_sessions)
        self.assertEqual(args.sessions_root, Path("codex-sessions"))
        self.assertEqual(args.days, 3)
        for argv in (
            ["--parent-sessions", "--runs-only"],
            ["--parent-sessions", "--listener-only"],
            ["--sessions-root", "codex-sessions"],
        ):
            with self.subTest(argv=argv), self.assertRaises(SystemExit):
                usage.parse_args(argv)

    def test_text_and_json_rendering_include_qoder_lane(self) -> None:
        runs = usage.RunUsage(
            days=1,
            runs=2,
            outcomes={"completed": 2},
            timeout_profiles={},
            read_only=2,
            network_allowed=0,
            requested_routes={},
            elapsed_seconds_total=1.0,
            qoder_lane={"attempted": 1, "not_recorded": 1},
        )
        json_output = io.StringIO()
        with redirect_stdout(json_output):
            usage.print_usage(None, runs, json_output=True)
        self.assertEqual(
            json.loads(json_output.getvalue())["runs"]["qoder_lane"],
            {"attempted": 1, "not_recorded": 1},
        )
        text_output = io.StringIO()
        with redirect_stdout(text_output):
            usage.print_usage(None, runs, json_output=False)
        self.assertIn(
            'qoder_lane: {"attempted": 1, "not_recorded": 1}',
            text_output.getvalue(),
        )

    def test_missing_runs_directory_is_not_reported_as_zero_runs(self) -> None:
        with (
            TemporaryDirectory() as directory,
            self.assertRaisesRegex(ValueError, "--runs-dir must be an existing"),
        ):
            usage.summarize_runs(Path(directory) / "missing", days=1)

    def test_mutuallyExclusiveModesAreRejected(self) -> None:
        with self.assertRaises(SystemExit):
            usage.parse_args(["--runs-only", "--listener-only"])


if __name__ == "__main__":
    unittest.main()
