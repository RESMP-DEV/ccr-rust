#!/usr/bin/env python3
"""Tests for the CCR idle-restart gate."""

from __future__ import annotations

import http.client
import io
import json
import unittest
from contextlib import redirect_stdout
from unittest import mock

import ccr_wait_idle

METRICS_TEMPLATE = """# HELP ccr_active_requests Current requests
# TYPE ccr_active_requests gauge
ccr_active_requests {requests}
# HELP ccr_active_streams Current streams
# TYPE ccr_active_streams gauge
ccr_active_streams {streams}
"""


class FakeResponse:
    def __init__(self, payload: str) -> None:
        self.payload = payload.encode("utf-8")
        self.status = 200

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, traceback):
        return False

    def read(self) -> bytes:
        return self.payload


class IdleRestartTests(unittest.TestCase):
    def test_parse_metrics_rejects_missing_gauges(self) -> None:
        with self.assertRaises(ValueError):
            ccr_wait_idle.parse_metrics("ccr_active_requests 0\n")

    def test_parse_metrics_accepts_labels_timestamps_and_floats(self) -> None:
        snapshot = ccr_wait_idle.parse_metrics(
            METRICS_TEMPLATE.format(requests=1, streams=0).replace(
                "ccr_active_requests 1",
                'ccr_active_requests{listener="main"} 1.5 1697212800000',
            )
        )

        self.assertEqual(snapshot.active_requests, 1.5)
        self.assertEqual(snapshot.active_streams, 0)

    def test_parse_metrics_rejects_malformed_numbers(self) -> None:
        with self.assertRaises(ValueError):
            ccr_wait_idle.parse_metrics(
                "ccr_active_requests 1.2.3\nccr_active_streams 0\n"
            )

    def test_wait_requires_consecutive_idle_samples(self) -> None:
        payloads = [
            METRICS_TEMPLATE.format(requests=1, streams=1),
            METRICS_TEMPLATE.format(requests=0, streams=0),
            METRICS_TEMPLATE.format(requests=0, streams=0),
        ]
        with mock.patch.object(
            ccr_wait_idle.urllib.request,
            "urlopen",
            side_effect=[FakeResponse(payload) for payload in payloads],
        ):
            snapshot, elapsed, samples = ccr_wait_idle.wait_until_idle(
                "http://127.0.0.1:3456/metrics",
                timeout=2,
                poll_interval=0,
                stable_samples=2,
                request_timeout=1,
            )

        self.assertEqual(snapshot.active_requests, 0)
        self.assertEqual(snapshot.active_streams, 0)
        self.assertEqual(samples, 3)
        self.assertGreaterEqual(elapsed, 0)

    def test_busy_listener_times_out_with_exit_75(self) -> None:
        payload = METRICS_TEMPLATE.format(requests=2, streams=1)
        with mock.patch.object(
            ccr_wait_idle.urllib.request,
            "urlopen",
            return_value=FakeResponse(payload),
        ):
            output = io.StringIO()
            with redirect_stdout(output):
                status = ccr_wait_idle.main(
                    [
                        "--url",
                        "http://127.0.0.1:3456/metrics",
                        "--timeout",
                        "0.01",
                        "--poll-interval",
                        "0",
                        "--json",
                    ]
                )

        self.assertEqual(status, 75)
        result = json.loads(output.getvalue())
        self.assertFalse(result["ok"])
        self.assertIn("stayed busy", result["error"])

    def test_recovered_transport_error_does_not_mask_a_busy_timeout(self) -> None:
        payload = METRICS_TEMPLATE.format(requests=2, streams=1)
        with (
            mock.patch.object(
                ccr_wait_idle.urllib.request,
                "urlopen",
                side_effect=[
                    http.client.BadStatusLine("non-HTTP response"),
                    *[FakeResponse(payload)] * 12,
                ],
            ),
            self.assertRaises(ccr_wait_idle.BusyListener) as context,
        ):
            ccr_wait_idle.wait_until_idle(
                "http://127.0.0.1:3456/metrics",
                timeout=0.03,
                poll_interval=0.005,
                stable_samples=2,
                request_timeout=1,
            )

        self.assertIn("listener stayed busy", str(context.exception))

    def test_stable_samples_argument_rejects_single_sample(self) -> None:
        with self.assertRaises(SystemExit) as context:
            ccr_wait_idle.parse_args(
                ["--url", "http://127.0.0.1/metrics", "--stable-samples", "1"]
            )

        self.assertEqual(context.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
