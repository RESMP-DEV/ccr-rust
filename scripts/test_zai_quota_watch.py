#!/usr/bin/env python3
"""Tests for the Z.AI coding-plan quota watcher.

The stub server answers both monitor endpoints from local fixtures. It listens
on a loopback socket where the environment permits one, and otherwise serves
the same http.server handler in-process, so no test ever reaches api.z.ai and
no test reads a real credential file.
"""

from __future__ import annotations

import io
import json
import os
import socket
import socketserver
import tempfile
import threading
import unittest
import urllib.request
from contextlib import redirect_stderr, redirect_stdout
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, cast
from unittest import mock
from urllib.parse import urlsplit

import zai_quota_watch as watch

QUOTA_FIXTURE = {
    "code": 200,
    "data": {
        "limits": [
            {
                "type": "TIME_LIMIT",
                "unit": 5,
                "number": 1,
                "usage": 4000,
                "currentValue": 34,
                "remaining": 3966,
                "percentage": 1,
                "nextResetTime": 1795922774998,
                "usageDetails": [{"modelCode": "search-prime", "usage": 20}],
            },
            {
                "type": "TOKENS_LIMIT",
                "unit": 3,
                "number": 5,
                "percentage": 8,
                "nextResetTime": 1795514966733,
            },
        ],
        "level": "max",
    },
}

QUOTA_FIXTURE_SECOND = {
    "code": 200,
    "data": {
        "limits": [
            {
                "type": "TIME_LIMIT",
                "unit": 5,
                "number": 1,
                "usage": 5200,
                "currentValue": 234,
                "remaining": 3766,
                "percentage": 2,
                "nextResetTime": 1795922774998,
                "usageDetails": [{"modelCode": "search-prime", "usage": 120}],
            },
            {
                "type": "TOKENS_LIMIT",
                "unit": 3,
                "number": 5,
                "percentage": 9,
                "nextResetTime": 1795514966733,
            },
        ],
        "level": "max",
    },
}

MODEL_FIXTURE = {
    "code": 200,
    "data": {
        "granularity": "hourly",
        "x_time": ["2026-11-24 13:00", "2026-11-24 14:00"],
        "modelCallCount": [175, 298],
        "tokensUsage": [12106099, 28621813],
        "totalUsage": {
            "totalModelCallCount": 16712,
            "totalTokensUsage": 2016961527,
            "modelSummaryList": [
                {"modelName": "GLM-5.4", "totalTokens": 279271645, "sortOrder": 1}
            ],
        },
        "modelDataList": [
            {
                "modelCode": "glm-5.4",
                "modelName": "GLM-5.4",
                "sortOrder": 1,
                "tokensUsage": [12106099, 28621813],
                "totalTokens": 40727912,
            }
        ],
    },
}

MODEL_FIXTURE_SECOND = {
    "code": 200,
    "data": {
        "granularity": "hourly",
        "x_time": ["2026-11-24 13:00", "2026-11-24 14:00", "2026-11-24 15:00"],
        "modelCallCount": [175, 298, 120],
        "tokensUsage": [12106099, 28621813, 9000000],
        "totalUsage": {
            "totalModelCallCount": 17130,
            "totalTokensUsage": 2045461527,
            "modelSummaryList": [
                {"modelName": "GLM-5.4", "totalTokens": 288271645, "sortOrder": 1}
            ],
        },
        "modelDataList": [
            {
                "modelCode": "glm-5.4",
                "modelName": "GLM-5.4",
                "sortOrder": 1,
                "tokensUsage": [12106099, 28621813, 9000000],
                "totalTokens": 49727912,
            }
        ],
    },
}


MODEL_SUMMARY_FIXTURE = {
    "code": 200,
    "data": {
        "granularity": "hourly",
        "x_time": ["2026-11-24 13:00", "2026-11-24 14:00"],
        "modelCallCount": [175, 298],
        "tokensUsage": [12106099, 28621813],
        "totalUsage": {
            "totalModelCallCount": 16712,
            "totalTokensUsage": 2016961527,
        },
        "modelSummaryList": [
            {
                "modelCode": "glm-5.4",
                "modelName": "GLM-5.4",
                "totalTokens": 40727912,
                "tokensUsage": [12106099, 28621813],
                "callCount": [12, 20],
            }
        ],
    },
}


MODEL_SUMMARY_FIXTURE_SECOND = {
    "code": 200,
    "data": {
        "granularity": "hourly",
        "x_time": ["2026-11-24 13:00", "2026-11-24 14:00"],
        "modelCallCount": [175, 298],
        "tokensUsage": [12106099, 28621813],
        "totalUsage": {
            "totalModelCallCount": 17130,
            "totalTokensUsage": 2045461527,
        },
        "modelSummaryList": [
            {
                "modelCode": "glm-5.4",
                "modelName": "GLM-5.4",
                "totalTokens": 49727912,
                "tokensUsage": [12106099, 28621813],
                "callCount": [12, 35],
            }
        ],
    },
}


def build_handler(state: dict[str, Any]) -> type[BaseHTTPRequestHandler]:
    """Return a loopback stub handler that serves ``state["routes"]``."""

    class Handler(BaseHTTPRequestHandler):
        server_version = "ZaiQuotaWatchStub/1.0"

        def do_GET(self) -> None:
            path = urlsplit(self.path).path
            state["requests"].append((path, self.headers.get("authorization")))
            route = state["routes"].get(path)
            if route is None:
                self._send(404, {"error": f"no route for {path}"})
                return
            status, body = route
            self._send(status, body)

        def _send(self, status: int, body: Any) -> None:
            payload = json.dumps(body).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, format: str, *args: Any) -> None:
            return

    return Handler


class _FakeSocket:
    """Minimal socket stand-in that feeds a canned HTTP request to a handler."""

    def __init__(self, request_bytes: bytes) -> None:
        self._reader = io.BytesIO(request_bytes)
        self._writer = io.BytesIO()

    def makefile(self, mode: str, *args: Any, **kwargs: Any) -> io.BytesIO:
        return self._reader if "r" in mode else self._writer

    def sendall(self, data: bytes) -> None:
        self._writer.write(bytes(data))

    def settimeout(self, _timeout: float | None) -> None:
        return

    def close(self) -> None:
        return

    def fileno(self) -> int:
        return 0

    @property
    def written(self) -> bytes:
        return self._writer.getvalue()


class _StubResponse:
    """Minimal urlopen response stand-in."""

    def __init__(self, status: int, body: bytes) -> None:
        self.status = status
        self._body = body

    def read(self) -> bytes:
        return self._body

    def __enter__(self) -> _StubResponse:
        return self

    def __exit__(self, *args: Any) -> bool:
        return False


def _parse_response(raw: bytes) -> tuple[int, bytes]:
    """Split a serialized HTTP response into a status code and a body."""
    head, separator, body = raw.partition(b"\r\n\r\n")
    if not separator:
        return 0, raw
    status_line = head.split(b"\r\n", 1)[0].decode("latin-1")
    fields = status_line.split(" ", 2)
    if len(fields) < 2 or not fields[1].isdigit():
        return 0, body
    return int(fields[1]), body


def _describe_request(request: Any) -> tuple[str, str, dict[str, str]]:
    """Return the method, path, and headers of a urllib request."""
    if not isinstance(request, urllib.request.Request):
        raise TypeError(f"unsupported request type: {type(request).__name__}")
    parsed = urlsplit(request.full_url)
    if parsed.hostname != "127.0.0.1":
        raise AssertionError(f"refusing to contact {parsed.hostname}")
    path = parsed.path + (f"?{parsed.query}" if parsed.query else "")
    headers = {name: value for name, value in request.header_items()}
    return request.get_method(), path, headers


def _can_bind_loopback() -> bool:
    """Report whether this environment permits a loopback listening socket."""
    try:
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
    except OSError:
        return False
    return True


class InProcessTransport:
    """Serve the stub handler in-process where sockets cannot be bound."""

    def __init__(self, handler_class: type[BaseHTTPRequestHandler]) -> None:
        self._handler_class = handler_class
        self._server = socketserver.BaseServer(("127.0.0.1", 0), handler_class)

    def open(self, request: Any, timeout: Any = None) -> _StubResponse:
        method, path, headers = _describe_request(request)
        raw = f"{method} {path} HTTP/1.1\r\n".encode()
        for name, value in headers.items():
            raw += f"{name}: {value}\r\n".encode()
        raw += b"\r\n"
        fake = _FakeSocket(raw)
        self._handler_class(cast(Any, fake), ("127.0.0.1", 0), self._server)
        status, body = _parse_response(fake.written)
        return _StubResponse(status, body)


class ZaiQuotaWatchTests(unittest.TestCase):
    """Exercise the watcher against a loopback stub of the monitor API."""

    def setUp(self) -> None:
        self.state: dict[str, Any] = {
            "routes": {
                watch.QUOTA_PATH: (200, QUOTA_FIXTURE),
                watch.MODEL_USAGE_PATH: (200, MODEL_FIXTURE),
            },
            "requests": [],
        }
        self.server: ThreadingHTTPServer | None = None
        self.thread: threading.Thread | None = None
        if _can_bind_loopback():
            server = ThreadingHTTPServer(("127.0.0.1", 0), build_handler(self.state))
            self.server = server
            self.thread = threading.Thread(target=server.serve_forever, daemon=True)
            self.thread.start()
            self.addCleanup(self._stop_server)
            self.origin = f"http://127.0.0.1:{server.server_address[1]}"
        else:
            transport = InProcessTransport(build_handler(self.state))
            patcher = mock.patch.object(
                watch.urllib.request, "urlopen", side_effect=transport.open
            )
            patcher.start()
            self.addCleanup(patcher.stop)
            self.origin = "http://127.0.0.1:1"
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.key_file = Path(self.tmp.name) / "credentials.json"
        self.key_file.write_text(
            json.dumps({watch.KEY_FIELD: "test-key-123"}), encoding="utf-8"
        )

    def _stop_server(self) -> None:
        if self.server is None or self.thread is None:
            return
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)

    def _monitor(self) -> watch.ZaiMonitor:
        return watch.ZaiMonitor(self.origin, "test-key-123", timeout=5)

    def _window(self) -> tuple[datetime, datetime]:
        window_end = datetime.now(timezone.utc)
        return window_end - timedelta(hours=1), window_end

    def _cli(self, *args: str) -> list[str]:
        """Return CLI arguments wired to the stub server and the temp key."""
        return [*args, "--key-file", str(self.key_file), "--origin", self.origin]

    def test_resolve_api_key_prefers_the_key_file(self) -> None:
        resolved = watch.resolve_api_key(
            self.key_file, {watch.KEY_FIELD: "env-key-456"}
        )
        self.assertEqual(resolved, "test-key-123")

    def test_resolve_api_key_falls_back_to_the_environment(self) -> None:
        missing = Path(self.tmp.name) / "missing.json"
        resolved = watch.resolve_api_key(missing, {watch.KEY_FIELD: "env-key-456"})
        self.assertEqual(resolved, "env-key-456")

    def test_resolve_api_key_reads_a_nested_credentials_object(self) -> None:
        nested = Path(self.tmp.name) / "nested.json"
        nested.write_text(
            json.dumps({"credentials": {watch.KEY_FIELD: "nested-key"}}),
            encoding="utf-8",
        )
        self.assertEqual(watch.resolve_api_key(nested, {}), "nested-key")

    def test_resolve_api_key_without_any_source_is_a_usage_error(self) -> None:
        missing = Path(self.tmp.name) / "missing.json"
        with self.assertRaises(watch.UsageError) as caught:
            watch.resolve_api_key(missing, {})
        self.assertIn(watch.KEY_FIELD, str(caught.exception))

    def test_resolve_api_key_rejects_a_malformed_key_file(self) -> None:
        broken = Path(self.tmp.name) / "broken.json"
        broken.write_text("{not json", encoding="utf-8")
        with self.assertRaises(watch.UsageError):
            watch.resolve_api_key(broken, {watch.KEY_FIELD: "env-key-456"})

    def test_quota_payload_parses_limits_including_one_without_percentage(self) -> None:
        parsed = watch.parse_quota_payload(QUOTA_FIXTURE)
        self.assertEqual(parsed["level"], "max")
        self.assertEqual(len(parsed["limits"]), 2)
        first = parsed["limits"][0]
        self.assertEqual(first["key"], "TIME_LIMIT|5")
        self.assertEqual(first["percentage"], 1.0)
        self.assertEqual(first["current_value"], 34.0)
        self.assertEqual(first["usage_details"], {"search-prime": 20.0})
        second = parsed["limits"][1]
        self.assertEqual(second["key"], "TOKENS_LIMIT|3")
        self.assertEqual(second["percentage"], 8.0)
        self.assertIsNone(second["current_value"])
        self.assertEqual(second["usage_details"], {})

    def test_quota_payload_tolerates_missing_fields(self) -> None:
        parsed = watch.parse_quota_payload({"code": 200})
        self.assertEqual(parsed["limits"], [])
        self.assertIsNone(parsed["level"])

        parsed = watch.parse_quota_payload(
            {"data": {"limits": [{"type": "weekly", "unit": "call"}]}}
        )
        self.assertEqual(len(parsed["limits"]), 1)
        self.assertIsNone(parsed["limits"][0]["percentage"])
        self.assertIsNone(parsed["limits"][0]["current_value"])
        self.assertEqual(parsed["limits"][0]["usage_details"], {})

        parsed = watch.parse_quota_payload(
            {"data": {"limits": [{"type": "TIME_LIMIT", "unit": 5, "number": 1}]}}
        )
        self.assertEqual(parsed["limits"][0]["key"], "TIME_LIMIT|5")
        parsed = watch.parse_quota_payload(
            {"data": {"limits": [{"type": "RATE_LIMIT", "unit": 0}]}}
        )
        self.assertEqual(parsed["limits"][0]["key"], "RATE_LIMIT|0")

    def test_model_usage_payload_summarizes_models(self) -> None:
        parsed = watch.parse_model_usage_payload(MODEL_FIXTURE)
        self.assertEqual(sorted(parsed["models"]), ["glm-5.4"])
        model = parsed["models"]["glm-5.4"]
        self.assertEqual(model["model"], "glm-5.4")
        self.assertEqual(model["model_name"], "GLM-5.4")
        self.assertEqual(model["window_total_tokens"], 40727912.0)
        self.assertEqual(model["last_hour_tokens"], 28621813.0)
        self.assertEqual(model["delta_from_previous_hour"], 16515714.0)
        self.assertIsNone(model["last_hour_calls"])
        self.assertEqual(parsed["total_usage"]["tokens"], 2016961527.0)
        self.assertEqual(parsed["total_usage"]["calls"], 16712.0)
        self.assertEqual(parsed["total_usage"]["last_hour_tokens"], 28621813.0)
        self.assertEqual(parsed["total_usage"]["last_hour_calls"], 298.0)
        self.assertEqual(parsed["granularity"], "hourly")
        self.assertEqual(parsed["x_time"], ["2026-11-24 13:00", "2026-11-24 14:00"])

    def test_model_usage_payload_keys_models_by_model_code(self) -> None:
        parsed = watch.parse_model_usage_payload(
            {
                "code": 200,
                "data": {
                    "granularity": "hourly",
                    "x_time": ["2026-11-24 13:00", "2026-11-24 14:00"],
                    "modelCallCount": [175, 298],
                    "tokensUsage": [12106099, 28621813],
                    "totalUsage": {
                        "totalModelCallCount": 16712,
                        "totalTokensUsage": 2016961527,
                    },
                    "modelDataList": [
                        {
                            "modelCode": "glm-5.4",
                            "modelName": "GLM-5.4",
                            "sortOrder": 1,
                            "tokensUsage": [12106099, 28621813],
                            "totalTokens": 40727912,
                        },
                        {
                            "modelCode": "glm-5.3",
                            "modelName": "GLM-5.3",
                            "sortOrder": 2,
                            "tokensUsage": [1000, 2000],
                            "totalTokens": 3000,
                        },
                    ],
                },
            }
        )
        self.assertEqual(sorted(parsed["models"]), ["glm-5.3", "glm-5.4"])
        self.assertEqual(parsed["models"]["glm-5.3"]["window_total_tokens"], 3000.0)
        self.assertEqual(parsed["models"]["glm-5.3"]["last_hour_tokens"], 2000.0)
        self.assertEqual(
            parsed["models"]["glm-5.3"]["delta_from_previous_hour"], 1000.0
        )
        self.assertEqual(parsed["models"]["glm-5.4"]["window_total_tokens"], 40727912.0)

    def test_model_usage_payload_tolerates_a_missing_model_summary_list(self) -> None:
        parsed = watch.parse_model_usage_payload(
            {"code": 200, "data": {"totalUsage": {"totalTokens": 5}}}
        )
        self.assertEqual(parsed["models"], {})
        self.assertEqual(parsed["total_usage"]["tokens"], 5.0)
        self.assertIsNone(parsed["total_usage"]["calls"])

    def test_model_usage_payload_accepts_the_live_model_name_shape(self) -> None:
        """A summary-only payload has no modelCode and no per-model series."""
        parsed = watch.parse_model_usage_payload(
            {
                "code": 200,
                "success": True,
                "data": {
                    "x_time": ["2026-10-25", "2026-10-26"],
                    "modelCallCount": [3, 4],
                    "tokensUsage": [100, 200],
                    "modelSummaryList": [
                        {
                            "modelName": "GLM-5.4",
                            "totalTokens": 279271645,
                            "sortOrder": 1,
                        },
                        {
                            "modelName": "GLM-5.3",
                            "totalTokens": 30470778175,
                            "sortOrder": 2,
                        },
                    ],
                    "totalUsage": {
                        "totalModelCallCount": 194465,
                        "totalTokensUsage": 32519591788,
                        "modelSummaryList": [
                            {"modelName": "GLM-5.4", "totalTokens": 279271645}
                        ],
                    },
                },
            }
        )
        self.assertEqual(sorted(parsed["models"]), ["GLM-5.3", "GLM-5.4"])
        self.assertEqual(
            parsed["models"]["GLM-5.4"]["window_total_tokens"], 279271645.0
        )
        self.assertIsNone(parsed["models"]["GLM-5.4"]["last_hour_tokens"])
        self.assertIsNone(parsed["models"]["GLM-5.4"]["delta_from_previous_hour"])
        self.assertIsNone(parsed["models"]["GLM-5.4"]["last_hour_calls"])
        self.assertEqual(parsed["total_usage"]["tokens"], 32519591788.0)
        self.assertEqual(parsed["total_usage"]["calls"], 194465.0)
        self.assertEqual(parsed["total_usage"]["last_hour_tokens"], 200.0)
        self.assertEqual(parsed["total_usage"]["last_hour_calls"], 4.0)

    def test_model_summary_payload_carries_per_model_call_deltas(self) -> None:
        """The summary-only shape keeps per-model token and call deltas."""
        self.state["routes"][watch.MODEL_USAGE_PATH] = (200, MODEL_SUMMARY_FIXTURE)
        monitor = self._monitor()
        window_start, window_end = self._window()
        first, state = watch.capture_snapshot(
            monitor, window_start=window_start, window_end=window_end
        )
        model = first["model_usage"]["models"]["glm-5.4"]
        self.assertEqual(model["model"], "glm-5.4")
        self.assertEqual(model["model_name"], "GLM-5.4")
        self.assertEqual(model["window_total_tokens"], 40727912.0)
        self.assertEqual(model["last_hour_tokens"], 28621813.0)
        self.assertEqual(model["delta_from_previous_hour"], 16515714.0)
        self.assertEqual(model["last_hour_calls"], 20.0)

        self.state["routes"][watch.MODEL_USAGE_PATH] = (
            200,
            MODEL_SUMMARY_FIXTURE_SECOND,
        )
        second, _ = watch.capture_snapshot(
            monitor,
            window_start=window_start,
            window_end=window_end,
            previous_state=state,
        )
        deltas = second["deltas"]["models"]["per_model"]["glm-5.4"]
        self.assertEqual(deltas["token_delta"], 9000000.0)
        self.assertEqual(deltas["last_hour_token_delta"], 0.0)
        self.assertEqual(deltas["call_delta"], 15.0)

    def test_snapshot_records_models_keyed_by_model_code(self) -> None:
        """The emitted record carries per-model series, not an empty object."""
        monitor = self._monitor()
        window_start, window_end = self._window()
        record, _ = watch.capture_snapshot(
            monitor, window_start=window_start, window_end=window_end
        )
        self.assertEqual(record["errors"], [])
        self.assertEqual(sorted(record["model_usage"]["models"]), ["glm-5.4"])
        self.assertEqual(
            record["model_usage"]["models"]["glm-5.4"],
            {
                "model": "glm-5.4",
                "model_name": "GLM-5.4",
                "window_total_tokens": 40727912.0,
                "last_hour_tokens": 28621813.0,
                "delta_from_previous_hour": 16515714.0,
                "last_hour_calls": None,
            },
        )
        self.assertEqual(
            record["model_usage"]["x_time"],
            ["2026-11-24 13:00", "2026-11-24 14:00"],
        )
        # Call counts exist only in aggregate, recorded at the top level.
        self.assertEqual(record["model_usage"]["total_usage"]["calls"], 16712.0)
        self.assertEqual(record["model_usage"]["total_usage"]["last_hour_calls"], 298.0)

    def test_deltas_are_computed_against_the_previous_poll(self) -> None:
        monitor = self._monitor()
        window_start, window_end = self._window()
        first, state = watch.capture_snapshot(
            monitor, window_start=window_start, window_end=window_end
        )
        self.assertIsNone(
            first["deltas"]["models"]["per_model"]["glm-5.4"]["token_delta"]
        )
        self.assertIsNone(
            first["deltas"]["quota"]["limits"]["TIME_LIMIT|5"]["percentage_delta"]
        )

        self.state["routes"][watch.QUOTA_PATH] = (200, QUOTA_FIXTURE_SECOND)
        self.state["routes"][watch.MODEL_USAGE_PATH] = (200, MODEL_FIXTURE_SECOND)
        second, _ = watch.capture_snapshot(
            monitor,
            window_start=window_start,
            window_end=window_end,
            previous_state=state,
        )
        quota_deltas = second["deltas"]["quota"]["limits"]
        model_deltas = second["deltas"]["models"]["per_model"]
        aggregate_deltas = second["deltas"]["models"]["aggregate"]
        self.assertAlmostEqual(
            quota_deltas["TIME_LIMIT|5"]["percentage_delta"], 1.0, places=6
        )
        self.assertEqual(quota_deltas["TIME_LIMIT|5"]["current_value_delta"], 200.0)
        self.assertEqual(
            quota_deltas["TIME_LIMIT|5"]["usage_detail_deltas"]["search-prime"], 100.0
        )
        self.assertEqual(quota_deltas["TOKENS_LIMIT|3"]["percentage_delta"], 1.0)
        self.assertIsNone(quota_deltas["TOKENS_LIMIT|3"]["current_value_delta"])
        self.assertEqual(model_deltas["glm-5.4"]["token_delta"], 9000000.0)
        self.assertEqual(model_deltas["glm-5.4"]["last_hour_token_delta"], -19621813.0)
        self.assertIsNone(model_deltas["glm-5.4"]["call_delta"])
        self.assertEqual(aggregate_deltas["token_delta"], 28500000.0)
        self.assertEqual(aggregate_deltas["call_delta"], 418.0)
        self.assertEqual(aggregate_deltas["last_hour_call_delta"], -178.0)
        self.assertIsNotNone(second["deltas"]["quota"]["interval_seconds"])

    def test_non_200_response_is_recorded_as_an_api_error(self) -> None:
        self.state["routes"][watch.QUOTA_PATH] = (503, {"error": "unavailable"})
        monitor = self._monitor()
        window_start, window_end = self._window()
        record, _ = watch.capture_snapshot(
            monitor, window_start=window_start, window_end=window_end
        )
        self.assertFalse(record["quota"]["ok"])
        self.assertIsNone(record["quota"]["status"])
        self.assertIn("HTTP 503", record["quota"]["error"])
        self.assertNotIn("test-key-123", record["quota"]["error"])
        self.assertEqual(record["errors"][0]["scope"], "quota")
        self.assertTrue(record["model_usage"]["ok"])

    def test_api_key_is_sent_raw_and_never_recorded(self) -> None:
        monitor = self._monitor()
        window_start, window_end = self._window()
        record, _ = watch.capture_snapshot(
            monitor, window_start=window_start, window_end=window_end
        )
        headers = [auth for _path, auth in self.state["requests"]]
        self.assertEqual(headers, ["test-key-123", "test-key-123"])
        self.assertNotIn("test-key-123", json.dumps(record))

    def test_once_mode_prints_one_json_snapshot(self) -> None:
        buffer = io.StringIO()
        with redirect_stdout(buffer):
            code = watch.main(self._cli("--once"))
        self.assertEqual(code, 0)
        record = json.loads(buffer.getvalue())
        self.assertEqual(record["mode"], "once")
        self.assertTrue(record["quota"]["ok"])
        self.assertTrue(record["model_usage"]["ok"])
        self.assertEqual(record["errors"], [])
        self.assertIn("captured_at", record)

    def test_once_mode_returns_three_on_an_api_error(self) -> None:
        self.state["routes"][watch.QUOTA_PATH] = (500, {"error": "boom"})
        buffer = io.StringIO()
        with redirect_stdout(buffer):
            code = watch.main(self._cli("--once"))
        self.assertEqual(code, 3)
        self.assertTrue(json.loads(buffer.getvalue())["errors"])

    def test_missing_key_returns_two(self) -> None:
        missing = str(Path(self.tmp.name) / "missing.json")
        with (
            mock.patch.dict(os.environ, {}, clear=True),
            redirect_stderr(io.StringIO()),
        ):
            code = watch.main(
                ["--once", "--key-file", missing, "--origin", self.origin]
            )
        self.assertEqual(code, 2)

    def test_help_states_what_the_script_does_not_prove(self) -> None:
        text = watch.build_parser().format_help()
        self.assertIn("does NOT prove", text)
        self.assertIn("/api/monitor/usage/quota/limit", text)
        self.assertIn("/api/monitor/usage/model-usage", text)

    def test_watch_mode_appends_records_to_the_output_file(self) -> None:
        out_path = Path(self.tmp.name) / "nested" / "watch.jsonl"
        args = watch.build_parser().parse_args(
            [
                "--watch",
                "--interval",
                "0.01",
                "--out",
                str(out_path),
                "--key-file",
                str(self.key_file),
                "--origin",
                self.origin,
            ]
        )
        monitor = self._monitor()

        def stop_after_one_poll(_seconds: float) -> None:
            raise KeyboardInterrupt

        with (
            mock.patch.object(watch.time, "sleep", side_effect=stop_after_one_poll),
            redirect_stderr(io.StringIO()),
        ):
            code = watch.watch_loop(monitor, args)
        self.assertEqual(code, 0)
        lines = out_path.read_text(encoding="utf-8").strip().splitlines()
        self.assertEqual(len(lines), 1)
        record = json.loads(lines[0])
        self.assertEqual(record["mode"], "watch")
        self.assertTrue(record["quota"]["ok"])
