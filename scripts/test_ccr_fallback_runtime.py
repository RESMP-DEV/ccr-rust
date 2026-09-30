"""Exercise the installed CCR binary against local upstreams, without credentials."""

import json
import os
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

import ccr_fallback_policy as policy


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class RuntimeTests(unittest.TestCase):
    def run_case(self, failures: int, all_limited: bool = False) -> None:
        binary = os.environ.get("CCR_TEST_BINARY") or shutil.which("ccr-rust")
        if not binary:
            self.skipTest("Set CCR_TEST_BINARY or install ccr-rust")
        requests = []

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, format: str, *args: Any) -> None:
                pass

            def do_POST(self) -> None:
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                model = body["model"]
                requests.append((model, self.path, body, time.monotonic()))
                index = ["flashx", "flash", "full", "last"].index(model)
                limited = all_limited or index < failures
                if limited:
                    response = {
                        "error": {
                            "type": "rate_limit_error",
                            "message": "fixture capacity limit",
                        }
                    }
                elif model == "last":
                    response = {
                        "id": "msg_fixture",
                        "type": "message",
                        "role": "assistant",
                        "model": model,
                        "content": [{"type": "text", "text": "FALLBACK_OK"}],
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 5, "output_tokens": 3},
                    }
                else:
                    response = {
                        "id": "resp_fixture",
                        "object": "response",
                        "status": "completed",
                        "model": model,
                        "output": [
                            {
                                "id": "msg_fixture",
                                "type": "message",
                                "role": "assistant",
                                "status": "completed",
                                "content": [
                                    {
                                        "type": "output_text",
                                        "text": "FALLBACK_OK",
                                        "annotations": [],
                                    }
                                ],
                            }
                        ],
                        "usage": {
                            "input_tokens": 5,
                            "output_tokens": 3,
                            "total_tokens": 8,
                        },
                    }
                data = json.dumps(response).encode()
                self.send_response(429 if limited else 200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                if limited:
                    self.send_header("Retry-After", "1")
                self.end_headers()
                self.wfile.write(data)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            port = free_port()
            endpoint = f"http://127.0.0.1:{server.server_port}"
            config = {
                "Providers": [
                    {
                        "name": "fast",
                        "models": ["flashx", "flash", "full"],
                        "protocol": "responses",
                        "api_base_url": endpoint + "/v1",
                        "api_key": "fixture",
                    }
                ],
                "Router": {
                    "default": "fast,flashx",
                    "retrySweeps": {
                        "enabled": True,
                        "maxSweeps": 1,
                        "sweepCooldownMs": 50,
                        "maxHoldMs": 1500,
                    },
                },
                "Persistence": {"mode": "none"},
                "DebugCapture": {"enabled": False},
                "HOST": "127.0.0.1",
                "PORT": port,
            }
            shared = {
                "routes": ["fast,flashx", "fast,flash", "fast,full", "last,last"],
                "consumers": {"worker.json": {"primary": "fast,flashx"}},
                "providers": [
                    {
                        "name": "last",
                        "models": ["last"],
                        "protocol": "anthropic",
                        "api_base_url": endpoint + "/v1",
                        "api_key": "${FIXTURE_KEY}",
                        "transformer": {"use": [["maxtoken", {"max_tokens": 32768}]]},
                    }
                ],
            }
            policy.atomic_json(
                root / "config.json", policy.derive(config, shared, "worker.json")
            )
            try:
                with (root / "router.log").open("w+") as log:
                    process = subprocess.Popen(
                        [
                            binary,
                            "--config",
                            str(root / "config.json"),
                            "start",
                            "--host",
                            "127.0.0.1",
                            "--port",
                            str(port),
                        ],
                        stdout=log,
                        stderr=log,
                        env={**os.environ, "FIXTURE_KEY": "fixture"},
                    )
                    try:
                        for _ in range(100):
                            try:
                                with urllib.request.urlopen(
                                    f"http://127.0.0.1:{port}/health", timeout=0.2
                                ):
                                    break
                            except (OSError, urllib.error.URLError):
                                if process.poll() is not None:
                                    log.seek(0)
                                    self.fail(log.read()[-3000:])
                                time.sleep(0.05)
                        body = {
                            "model": "fast,flashx",
                            "input": "Return FALLBACK_OK",
                            "reasoning": {"effort": "max"},
                            "stream": False,
                        }
                        request = urllib.request.Request(
                            f"http://127.0.0.1:{port}/v1/responses",
                            data=json.dumps(body).encode(),
                            headers={"Content-Type": "application/json"},
                        )
                        started = time.monotonic()
                        try:
                            with urllib.request.urlopen(
                                request, timeout=15
                            ) as response:
                                status, result = (
                                    response.status,
                                    response.read().decode(),
                                )
                        except urllib.error.HTTPError as exc:
                            status, result = exc.code, exc.read().decode()
                        if all_limited:
                            self.assertEqual(status, 429, result)
                            self.assertGreater(time.monotonic() - started, 0.8)
                            self.assertEqual(
                                [r[0] for r in requests[:4]],
                                ["flashx", "flash", "full", "last"],
                            )
                            if len(requests) > 4:
                                self.assertGreater(requests[4][3] - requests[0][3], 0.8)
                        else:
                            self.assertEqual(status, 200, result)
                            self.assertIn("FALLBACK_OK", result)
                            self.assertEqual(
                                [r[0] for r in requests],
                                ["flashx", "flash", "full", "last"][: failures + 1],
                            )
                            for _, path, upstream, _ in requests:
                                if path.endswith("/responses"):
                                    self.assertEqual(
                                        upstream["reasoning"]["effort"], "max"
                                    )
                                else:
                                    self.assertEqual(
                                        upstream["output_config"]["effort"], "max"
                                    )
                                    self.assertEqual(upstream["max_tokens"], 32768)
                    finally:
                        process.terminate()
                        process.wait(timeout=10)
            finally:
                server.shutdown()
                server.server_close()
                thread.join(timeout=2)

    def test_preference_chain(self) -> None:
        for failures in range(4):
            with self.subTest(failures=failures):
                self.run_case(failures)

    def test_exhaustion_waits_for_retry_after(self) -> None:
        self.run_case(4, all_limited=True)


if __name__ == "__main__":
    unittest.main()
