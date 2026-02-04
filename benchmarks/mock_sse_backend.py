#!/usr/bin/env python3
"""Mock SSE backend that simulates an LLM provider's /chat/completions endpoint.

Emits configurable SSE chunks with tunable latency to stress-test ccr-rust's
streaming proxy without requiring real API keys or burning tokens.

Usage:
    uv run python benchmarks/mock_sse_backend.py [--port 9999] [--chunks 20] [--delay-ms 50]
"""

from __future__ import annotations

import argparse
import asyncio
import json
import time

import uvicorn
from starlette.applications import Starlette
from starlette.requests import Request
from starlette.responses import StreamingResponse
from starlette.routing import Route

DEFAULT_PORT = 9999
DEFAULT_CHUNKS = 20
DEFAULT_DELAY_MS = 50


def make_sse_chunk(index: int, text: str) -> str:
    """Format a single SSE data line matching Anthropic/OpenAI streaming format."""
    payload = {
        "id": f"chatcmpl-mock-{index}",
        "object": "chat.completion.chunk",
        "created": int(time.time()),
        "model": "mock-model",
        "choices": [
            {
                "index": 0,
                "delta": {"content": text},
                "finish_reason": None,
            }
        ],
    }
    return f"data: {json.dumps(payload)}\n\n"


def make_final_chunk() -> str:
    """The terminal [DONE] sentinel."""
    return "data: [DONE]\n\n"


def build_app(chunks: int, delay_ms: int) -> Starlette:
    """Build the Starlette ASGI app with the given streaming parameters."""

    delay_s = delay_ms / 1000.0
    words = [
        "The", "quick", "brown", "fox", "jumps", "over", "the", "lazy", "dog.",
        "Pack", "my", "box", "with", "five", "dozen", "liquor", "jugs.",
        "How", "vexingly", "quick", "daft", "zebras", "jump.",
    ]

    async def chat_completions(request: Request) -> StreamingResponse:
        body = await request.json()
        is_stream = body.get("stream", False)

        if not is_stream:
            # Non-streaming: return a complete response
            response = {
                "id": "chatcmpl-mock-batch",
                "object": "chat.completion",
                "created": int(time.time()),
                "model": "mock-model",
                "choices": [
                    {
                        "index": 0,
                        "message": {"role": "assistant", "content": "Hello from mock backend."},
                        "finish_reason": "stop",
                    }
                ],
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 5,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0,
                },
            }
            return StreamingResponse(
                iter([json.dumps(response)]),
                media_type="application/json",
                status_code=200,
            )

        # Streaming: emit SSE chunks with delay
        async def generate():
            for i in range(chunks):
                word = words[i % len(words)]
                yield make_sse_chunk(i, word + " ")
                await asyncio.sleep(delay_s)
            yield make_final_chunk()

        return StreamingResponse(
            generate(),
            media_type="text/event-stream",
            headers={
                "Cache-Control": "no-cache",
                "Connection": "keep-alive",
                "X-Mock-Chunks": str(chunks),
                "X-Mock-Delay-Ms": str(delay_ms),
            },
        )

    async def health(request: Request) -> StreamingResponse:
        return StreamingResponse(iter(["ok"]), media_type="text/plain")

    routes = [
        Route("/chat/completions", chat_completions, methods=["POST"]),
        Route("/health", health, methods=["GET"]),
    ]

    return Starlette(routes=routes)


def main() -> None:
    parser = argparse.ArgumentParser(description="Mock SSE backend for ccr-rust benchmarks")
    parser.add_argument("--port", type=int, default=DEFAULT_PORT, help="Listen port")
    parser.add_argument("--chunks", type=int, default=DEFAULT_CHUNKS, help="SSE chunks per response")
    parser.add_argument("--delay-ms", type=int, default=DEFAULT_DELAY_MS, help="Delay between chunks (ms)")
    parser.add_argument("--host", default="127.0.0.1", help="Listen address")
    args = parser.parse_args()

    app = build_app(args.chunks, args.delay_ms)

    print(f"Mock SSE backend starting on {args.host}:{args.port}")
    print(f"  chunks={args.chunks}, delay={args.delay_ms}ms per chunk")
    print(f"  Total stream duration per request: ~{args.chunks * args.delay_ms}ms")

    uvicorn.run(app, host=args.host, port=args.port, log_level="warning")


if __name__ == "__main__":
    main()
