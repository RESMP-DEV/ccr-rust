#!/usr/bin/env python3
"""Tests for CCR worker usage summaries."""

from __future__ import annotations

import json
import unittest
from datetime import date, timedelta
from pathlib import Path
from tempfile import TemporaryDirectory

import ccr_worker_usage as usage


class WorkerUsageTests(unittest.TestCase):
    def test_parse_listener_metrics_reads_active_and_tier_counters(self) -> None:
        metrics = """# HELP ccr_active_requests Current
ccr_active_requests 2
ccr_requests_total{tier="zai"} 28
ccr_requests_total{tier="minimax"} 1
ccr_requests_total{tier="zai"} 4
"""
        result = usage.parse_listener_metrics(metrics, "http://127.0.0.1:3457/metrics")
        self.assertEqual(result.active_requests, 2)
        self.assertEqual(result.tier_requests, {"minimax": 1, "zai": 4})

    def test_nonloopback_listener_is_rejected(self) -> None:
        for url in ("https://127.0.0.1/metrics", "http://example.test/metrics"):
            with self.assertRaises(ValueError):
                usage.validate_local_http_url(url)

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
            ]
            for name, status in records:
                run_dir = root / name
                run_dir.mkdir()
                (run_dir / "status.json").write_text(
                    json.dumps(status), encoding="utf-8"
                )

            result = usage.summarize_runs(root, days=1)

        self.assertEqual(result.runs, 2)
        self.assertEqual(result.outcomes, {"completed": 1, "timed_out": 1})
        self.assertEqual(result.timeout_profiles, {"review": 2})
        self.assertEqual(result.requested_routes, {"zai,glm-5.3-flash": 2})
        self.assertEqual((result.read_only, result.network_allowed), (2, 1))
        self.assertEqual(result.elapsed_seconds_total, 15.0)

    def test_mutuallyExclusiveModesAreRejected(self) -> None:
        with self.assertRaises(SystemExit):
            usage.parse_args(["--runs-only", "--listener-only"])


if __name__ == "__main__":
    unittest.main()
