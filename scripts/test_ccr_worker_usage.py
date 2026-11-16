#!/usr/bin/env python3
"""Tests for CCR worker usage summaries."""

from __future__ import annotations

import io
import json
import unittest
from contextlib import redirect_stdout
from datetime import date, timedelta
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
                    "attempted",
                    {"outcome": "completed", "qoder_primary_attempt": True},
                ),
                (
                    "attempted-false",
                    {"outcome": "completed", "qoder_primary_attempt": False},
                ),
                (
                    "attempted-precedence",
                    {
                        "outcome": "completed",
                        "qoder_primary_attempt": True,
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
                "network": 1,
                "not_recorded": 2,
            },
        )

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
