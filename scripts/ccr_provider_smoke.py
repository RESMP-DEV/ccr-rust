#!/usr/bin/env python3
"""CCR provider matrix smoke test.

Runs a live probe across configured provider routes and validates:
- /v1/chat/completions (stream=false and stream=true)
- /v1/responses (stream=false and stream=true)

Streaming validation checks SSE framing and JSON payload parseability.
The script exits non-zero if any probe fails.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from dataclasses import dataclass
from typing import Iterable, List, Tuple


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="CCR multi-provider smoke matrix")
    parser.add_argument(
        "--base-url",
        default="http://127.0.0.1:3456",
        help="CCR base URL (default: http://127.0.0.1:3456)",
    )
    parser.add_argument(
        "--api-key",
        default="test",
        help="Bearer token sent to CCR (default: test)",
    )
    parser.add_argument(
        "--timeout",
        type=int,
        default=60,
        help="Per-request timeout in seconds (default: 60)",
    )
    parser.add_argument(
        "--max-output-tokens",
        type=int,
        default=64,
        help="Max output tokens per request (default: 64)",
    )
    parser.add_argument(
        "--temperature",
        type=float,
        default=0.1,
        help="Temperature for chat requests (default: 0.1)",
    )
    parser.add_argument(
        "--prompt",
        default="Reply with exactly OK",
        help="Prompt used for probe requests",
    )
    parser.add_argument(
        "--models",
        default="",
        help="Comma-separated provider routes to test (e.g. zai,glm-4.7;deepseek,deepseek-chat). "
        "If omitted, routes are discovered from /v1/models.",
    )
    parser.add_argument(
        "--verbose",
        action="store_true",
        help="Print detailed response snippets for each case.",
    )
    return parser


def run_curl(
    url: str,
    payload: dict,
    api_key: str,
    timeout_s: int,
    streaming: bool,
) -> Tuple[int, str, str]:
    cmd: List[str] = [
        "curl",
        "-sS",
        "-m",
        str(timeout_s),
        "-w",
        "\nHTTP_STATUS:%{http_code}",
    ]
    if streaming:
        cmd.append("-N")
    cmd.extend(
        [
            "-H",
            "content-type: application/json",
            "-H",
            f"authorization: Bearer {api_key}",
            "-d",
            json.dumps(payload),
            url,
        ]
    )
    proc = subprocess.run(cmd, capture_output=True, text=True)
    stdout = proc.stdout or ""
    stderr = proc.stderr or ""

    if "\nHTTP_STATUS:" in stdout:
        body, status_str = stdout.rsplit("\nHTTP_STATUS:", 1)
        try:
            status = int(status_str.strip())
        except ValueError:
            status = 0
    else:
        body = stdout
        status = 0

    return status, body, stderr


def http_get_json(url: str, api_key: str, timeout_s: int) -> dict:
    cmd = [
        "curl",
        "-sS",
        "-m",
        str(timeout_s),
        "-H",
        f"authorization: Bearer {api_key}",
        url,
    ]
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if proc.returncode != 0:
        raise RuntimeError(f"GET {url} failed: {proc.stderr.strip()}")
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"GET {url} returned invalid JSON: {exc}") from exc


def discover_models(base_url: str, api_key: str, timeout_s: int) -> List[str]:
    models_url = f"{base_url.rstrip('/')}/v1/models"
    payload = http_get_json(models_url, api_key, timeout_s)
    data = payload.get("data")
    if not isinstance(data, list):
        raise RuntimeError("Unexpected /v1/models payload: missing data array")

    ids = []
    for entry in data:
        if not isinstance(entry, dict):
            continue
        model_id = entry.get("id")
        if isinstance(model_id, str) and "," in model_id:
            ids.append(model_id)

    deduped = []
    seen = set()
    for model_id in ids:
        if model_id not in seen:
            deduped.append(model_id)
            seen.add(model_id)
    return deduped


def parse_sse_frames(raw: str) -> List[str]:
    normalized = raw.replace("\r\n", "\n")
    return [frame for frame in normalized.split("\n\n") if frame.strip()]


def parse_sse_data(frame: str) -> str:
    data_lines = []
    for line in frame.split("\n"):
        if line.startswith("data:"):
            data_lines.append(line[5:].lstrip())
    return "\n".join(data_lines)


@dataclass
class ProbeResult:
    model: str
    case: str
    ok: bool
    latency_s: float
    detail: str


def run_chat_nonstream(
    base_url: str,
    model: str,
    api_key: str,
    timeout_s: int,
    prompt: str,
    max_tokens: int,
    temperature: float,
) -> ProbeResult:
    url = f"{base_url.rstrip('/')}/v1/chat/completions"
    payload = {
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": max_tokens,
        "stream": False,
        "temperature": temperature,
    }
    start = time.time()
    status, body, stderr = run_curl(url, payload, api_key, timeout_s, streaming=False)
    latency = time.time() - start

    if status != 200:
        msg = stderr.strip() or body[:220]
        return ProbeResult(model, "chat/non-stream", False, latency, f"HTTP {status}: {msg}")

    try:
        parsed = json.loads(body)
    except json.JSONDecodeError as exc:
        return ProbeResult(
            model,
            "chat/non-stream",
            False,
            latency,
            f"invalid JSON response: {exc}",
        )

    choices = parsed.get("choices")
    if not isinstance(choices, list) or not choices:
        return ProbeResult(
            model,
            "chat/non-stream",
            False,
            latency,
            "missing choices[0] in response",
        )

    return ProbeResult(model, "chat/non-stream", True, latency, "ok")


def run_chat_stream(
    base_url: str,
    model: str,
    api_key: str,
    timeout_s: int,
    prompt: str,
    max_tokens: int,
    temperature: float,
) -> ProbeResult:
    url = f"{base_url.rstrip('/')}/v1/chat/completions"
    payload = {
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": max_tokens,
        "stream": True,
        "temperature": temperature,
    }
    start = time.time()
    status, body, stderr = run_curl(url, payload, api_key, timeout_s, streaming=True)
    latency = time.time() - start

    if status != 200:
        msg = stderr.strip() or body[:220]
        return ProbeResult(model, "chat/stream", False, latency, f"HTTP {status}: {msg}")

    frames = parse_sse_frames(body)
    bad = 0
    done = 0
    for frame in frames:
        data = parse_sse_data(frame)
        if not data:
            continue
        if data.strip() == "[DONE]":
            done += 1
            continue
        try:
            json.loads(data)
        except json.JSONDecodeError:
            bad += 1

    if bad > 0:
        return ProbeResult(
            model,
            "chat/stream",
            False,
            latency,
            f"{bad} malformed JSON SSE frame(s)",
        )
    if done != 1:
        return ProbeResult(
            model,
            "chat/stream",
            False,
            latency,
            f"expected 1 [DONE] marker, got {done}",
        )
    return ProbeResult(model, "chat/stream", True, latency, f"ok ({len(frames)} frame(s))")


def run_responses_nonstream(
    base_url: str,
    model: str,
    api_key: str,
    timeout_s: int,
    prompt: str,
    max_tokens: int,
) -> ProbeResult:
    url = f"{base_url.rstrip('/')}/v1/responses"
    payload = {
        "model": model,
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": prompt}],
            }
        ],
        "max_output_tokens": max_tokens,
        "stream": False,
    }
    start = time.time()
    status, body, stderr = run_curl(url, payload, api_key, timeout_s, streaming=False)
    latency = time.time() - start

    if status != 200:
        msg = stderr.strip() or body[:220]
        return ProbeResult(
            model,
            "responses/non-stream",
            False,
            latency,
            f"HTTP {status}: {msg}",
        )

    try:
        parsed = json.loads(body)
    except json.JSONDecodeError as exc:
        return ProbeResult(
            model,
            "responses/non-stream",
            False,
            latency,
            f"invalid JSON response: {exc}",
        )

    if parsed.get("object") != "response":
        return ProbeResult(
            model,
            "responses/non-stream",
            False,
            latency,
            "missing object='response'",
        )
    return ProbeResult(model, "responses/non-stream", True, latency, "ok")


def run_responses_stream(
    base_url: str,
    model: str,
    api_key: str,
    timeout_s: int,
    prompt: str,
    max_tokens: int,
) -> ProbeResult:
    url = f"{base_url.rstrip('/')}/v1/responses"
    payload = {
        "model": model,
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": prompt}],
            }
        ],
        "max_output_tokens": max_tokens,
        "stream": True,
    }
    start = time.time()
    status, body, stderr = run_curl(url, payload, api_key, timeout_s, streaming=True)
    latency = time.time() - start

    if status != 200:
        msg = stderr.strip() or body[:220]
        return ProbeResult(
            model,
            "responses/stream",
            False,
            latency,
            f"HTTP {status}: {msg}",
        )

    frames = parse_sse_frames(body)
    bad = 0
    completed = 0
    failed = 0
    for frame in frames:
        data = parse_sse_data(frame)
        if not data:
            continue
        if data.strip() == "[DONE]":
            # Responses stream should not emit [DONE], but tolerate it if present.
            continue
        try:
            parsed = json.loads(data)
        except json.JSONDecodeError:
            bad += 1
            continue
        event_type = parsed.get("type")
        if event_type == "response.completed":
            completed += 1
        if event_type == "response.failed":
            failed += 1

    if bad > 0:
        return ProbeResult(
            model,
            "responses/stream",
            False,
            latency,
            f"{bad} malformed JSON SSE frame(s)",
        )
    if completed != 1:
        return ProbeResult(
            model,
            "responses/stream",
            False,
            latency,
            f"expected 1 response.completed event, got {completed}",
        )
    if failed > 0:
        return ProbeResult(
            model,
            "responses/stream",
            False,
            latency,
            f"received {failed} response.failed event(s)",
        )

    return ProbeResult(
        model,
        "responses/stream",
        True,
        latency,
        f"ok ({len(frames)} frame(s))",
    )


def parse_models_arg(raw: str) -> List[str]:
    if not raw.strip():
        return []
    models = []
    for token in raw.split(";"):
        model = token.strip()
        if model:
            models.append(model)
    return models


def print_results(results: Iterable[ProbeResult], verbose: bool) -> int:
    failures = 0
    for result in results:
        status = "PASS" if result.ok else "FAIL"
        if not result.ok:
            failures += 1
        print(
            f"[{status}] model={result.model} case={result.case} "
            f"latency={result.latency_s:.3f}s detail={result.detail}"
        )
        if verbose and result.ok:
            print(f"       verified {result.case} for {result.model}")
    return failures


def main() -> int:
    args = build_parser().parse_args()
    base_url = args.base_url.rstrip("/")

    models = parse_models_arg(args.models)
    if not models:
        models = discover_models(base_url, args.api_key, args.timeout)

    if not models:
        print("No provider routes discovered (expected ids like provider,model).", file=sys.stderr)
        return 2

    print(f"Discovered {len(models)} route(s): {', '.join(models)}")

    results: List[ProbeResult] = []
    for model in models:
        results.append(
            run_chat_nonstream(
                base_url,
                model,
                args.api_key,
                args.timeout,
                args.prompt,
                args.max_output_tokens,
                args.temperature,
            )
        )
        results.append(
            run_chat_stream(
                base_url,
                model,
                args.api_key,
                args.timeout,
                args.prompt,
                args.max_output_tokens,
                args.temperature,
            )
        )
        results.append(
            run_responses_nonstream(
                base_url,
                model,
                args.api_key,
                args.timeout,
                args.prompt,
                args.max_output_tokens,
            )
        )
        results.append(
            run_responses_stream(
                base_url,
                model,
                args.api_key,
                args.timeout,
                args.prompt,
                args.max_output_tokens,
            )
        )

    failures = print_results(results, args.verbose)
    passed = len(results) - failures
    print(f"Summary: {passed}/{len(results)} checks passed")

    return 0 if failures == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
