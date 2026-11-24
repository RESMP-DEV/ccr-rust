#!/usr/bin/env python3
"""HTTP and SSE transport for the CCR per-model benchmark.

The harness always streams. Streaming is what makes time to first token
measurable, and it keeps the connection open long enough to attribute a
failure to a transport class (timeout, connection, protocol, HTTP status)
instead of a bare failure.
"""

from __future__ import annotations

import http.client
import json
import time
from collections.abc import Iterable, Iterator
from dataclasses import dataclass, field
from typing import Any, Protocol
from urllib.parse import urlsplit

import ccr_model_benchmark_suite as suite

# CCR reports the tier that served a response in `x-ccr-tier` and the routes
# that were rate limited in `x-ccr-rate-limited-tiers`. The second header is
# only present on a terminal 429, so its absence is not evidence that no
# tier was rate limited.
TIER_HEADER = "x-ccr-tier"
RATE_LIMITED_TIERS_HEADER = "x-ccr-rate-limited-tiers"


class HeaderReader(Protocol):
    """The part of an HTTP response the tier headers are read from."""

    def getheader(self, name: str) -> str | None: ...


@dataclass
class StreamOutcome:
    """One streamed chat completion and its transport measurements."""

    text: str
    ttft_seconds: float | None
    input_tokens: int | None
    output_tokens: int | None
    usage_source: str
    error_class: str | None
    error_detail: str | None
    wall_seconds: float = 0.0
    served_tier: str | None = None
    rate_limited_tiers: list[str] = field(default_factory=list)


def read_tier_headers(
    response: HeaderReader,
) -> tuple[str | None, list[str]]:
    """Read the served-tier and rate-limited-tier headers from a response.

    `x-ccr-tier` carries the configured display name of the tier that served
    the request. `x-ccr-rate-limited-tiers` carries the full routes that were
    rate limited, and is only set on a terminal 429.
    """
    served_tier = response.getheader(TIER_HEADER)
    served_tier = served_tier.strip() if served_tier else ""
    raw_limited = response.getheader(RATE_LIMITED_TIERS_HEADER)
    rate_limited = parse_rate_limited_routes(raw_limited) if raw_limited else []
    return (served_tier or None), rate_limited


def parse_rate_limited_routes(raw: str) -> list[str]:
    """Split the rate-limited-tier header into the routes it names.

    CCR joins the full `provider,model` routes with commas, so two rate
    limited routes read `zai,glm-5.4,minimax,MiniMax-M3.1-Flash`. The
    fragments are re-paired two by one to recover the routes. A tier whose
    route has no provider part is reported as a single fragment, and a
    header that mixes such a tier with a qualified route is ambiguous in
    the header format itself.
    """
    fragments = [part.strip() for part in raw.split(",") if part.strip()]
    return [
        ",".join(fragments[index : index + 2]) for index in range(0, len(fragments), 2)
    ]


def _open_connection(
    base_url: str, timeout_seconds: float
) -> tuple[http.client.HTTPConnection, str]:
    parts = urlsplit(base_url)
    host = parts.hostname or "127.0.0.1"
    port = parts.port or (443 if parts.scheme == "https" else 80)
    prefix = parts.path.rstrip("/")
    if parts.scheme == "https":
        connection: http.client.HTTPConnection = http.client.HTTPSConnection(
            host, port, timeout=timeout_seconds
        )
    else:
        connection = http.client.HTTPConnection(host, port, timeout=timeout_seconds)
    return connection, prefix


def _timed_lines(
    response: http.client.HTTPResponse, started: float
) -> Iterator[tuple[float, str]]:
    """Yield (monotonic timestamp, line) pairs as the response arrives."""
    while True:
        raw = response.readline()
        if not raw:
            return
        yield time.monotonic() - started, raw.decode("utf-8", "replace")


def _content_from_event(event: dict[str, Any]) -> str:
    choices = event.get("choices")
    if not isinstance(choices, list) or not choices:
        return ""
    first = choices[0]
    if not isinstance(first, dict):
        return ""
    delta = first.get("delta")
    if isinstance(delta, dict):
        content = delta.get("content")
        if isinstance(content, str):
            return content
    message = first.get("message")
    if isinstance(message, dict):
        content = message.get("content")
        if isinstance(content, str):
            return content
    return ""


def _tokens_from_usage(usage: dict[str, Any]) -> tuple[int | None, int | None]:
    input_tokens = usage.get("prompt_tokens")
    output_tokens = usage.get("completion_tokens")
    if input_tokens is None:
        input_tokens = usage.get("input_tokens")
    if output_tokens is None:
        output_tokens = usage.get("output_tokens")
    return (
        input_tokens if isinstance(input_tokens, int) else None,
        output_tokens if isinstance(output_tokens, int) else None,
    )


def consume_sse(lines: Iterable[tuple[float, str]]) -> StreamOutcome:
    """Parse an SSE byte stream into text, TTFT, and reported usage."""
    parts: list[str] = []
    data_lines: list[str] = []
    frame_timestamp: float | None = None
    ttft_seconds: float | None = None
    usage: dict[str, Any] | None = None

    def apply(payload: str, timestamp: float | None) -> None:
        nonlocal ttft_seconds, usage
        if payload.strip() == "[DONE]":
            return
        try:
            event = json.loads(payload)
        except json.JSONDecodeError:
            return
        if not isinstance(event, dict):
            return
        content = _content_from_event(event)
        if content:
            if ttft_seconds is None:
                ttft_seconds = timestamp
            parts.append(content)
        event_usage = event.get("usage")
        if isinstance(event_usage, dict) and event_usage:
            usage = event_usage

    for timestamp, raw in lines:
        line = raw.rstrip("\r\n")
        if line == "":
            if data_lines:
                apply("\n".join(data_lines), frame_timestamp)
                data_lines = []
                frame_timestamp = None
            continue
        if line.startswith("data:"):
            if frame_timestamp is None:
                frame_timestamp = timestamp
            data_lines.append(line[5:].lstrip())
    if data_lines:
        apply("\n".join(data_lines), frame_timestamp)

    text = "".join(parts)
    input_tokens: int | None = None
    output_tokens: int | None = None
    usage_source = "none"
    if usage is not None:
        input_tokens, output_tokens = _tokens_from_usage(usage)
        if output_tokens is not None:
            usage_source = "upstream"
    if output_tokens is None and text:
        output_tokens = suite.estimate_tokens(text)
        usage_source = "estimated"
    return StreamOutcome(
        text=text,
        ttft_seconds=ttft_seconds,
        input_tokens=input_tokens,
        output_tokens=output_tokens,
        usage_source=usage_source,
        error_class=None,
        error_detail=None,
    )


def stream_chat_completion(
    base_url: str,
    api_key: str,
    payload: dict[str, Any],
    timeout_seconds: float,
) -> StreamOutcome:
    """POST one chat completion and stream it back from the local listener."""
    started = time.monotonic()
    outcome = _stream_chat_completion(base_url, api_key, payload, timeout_seconds)
    outcome.wall_seconds = time.monotonic() - started
    return outcome


def _stream_chat_completion(
    base_url: str,
    api_key: str,
    payload: dict[str, Any],
    timeout_seconds: float,
) -> StreamOutcome:
    """Issue the request and translate transport failures into error classes."""
    connection, prefix = _open_connection(base_url, timeout_seconds)
    request_started = time.monotonic()
    body = json.dumps(payload).encode("utf-8")
    headers = {
        "content-type": "application/json",
        "authorization": f"Bearer {api_key}",
        "accept": "text/event-stream",
    }
    # Provenance read from the response headers must survive a stream that
    # dies afterwards, otherwise a mid-stream timeout silently becomes
    # "unknown provenance" and an otherwise clean route can never be reported
    # clean. The headers are assigned as soon as the response arrives and are
    # carried into every error branch below.
    served_tier: str | None = None
    rate_limited_tiers: list[str] = []
    try:
        connection.request(
            "POST", f"{prefix}/v1/chat/completions", body=body, headers=headers
        )
        response = connection.getresponse()
        served_tier, rate_limited_tiers = read_tier_headers(response)
        if response.status != 200:
            raw = response.read().decode("utf-8", "replace")
            return StreamOutcome(
                text="",
                ttft_seconds=None,
                input_tokens=None,
                output_tokens=None,
                usage_source="none",
                error_class="http_error",
                error_detail=f"HTTP {response.status}: {raw[:300]}",
                served_tier=served_tier,
                rate_limited_tiers=rate_limited_tiers,
            )
        outcome = consume_sse(_timed_lines(response, request_started))
        outcome.served_tier = served_tier
        outcome.rate_limited_tiers = rate_limited_tiers
    except TimeoutError:
        return StreamOutcome(
            text="",
            ttft_seconds=None,
            input_tokens=None,
            output_tokens=None,
            usage_source="none",
            error_class="timeout",
            error_detail=f"request exceeded {timeout_seconds:g}s",
            served_tier=served_tier,
            rate_limited_tiers=list(rate_limited_tiers),
        )
    except ConnectionError as error:
        return StreamOutcome(
            text="",
            ttft_seconds=None,
            input_tokens=None,
            output_tokens=None,
            usage_source="none",
            error_class="connection_error",
            error_detail=str(error),
            served_tier=served_tier,
            rate_limited_tiers=list(rate_limited_tiers),
        )
    except http.client.HTTPException as error:
        return StreamOutcome(
            text="",
            ttft_seconds=None,
            input_tokens=None,
            output_tokens=None,
            usage_source="none",
            error_class="protocol_error",
            error_detail=str(error),
            served_tier=served_tier,
            rate_limited_tiers=list(rate_limited_tiers),
        )
    except OSError as error:
        return StreamOutcome(
            text="",
            ttft_seconds=None,
            input_tokens=None,
            output_tokens=None,
            usage_source="none",
            error_class="network_error",
            error_detail=str(error),
            served_tier=served_tier,
            rate_limited_tiers=list(rate_limited_tiers),
        )
    else:
        return outcome
    finally:
        connection.close()
