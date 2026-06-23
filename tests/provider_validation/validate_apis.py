#!/usr/bin/env python3
"""
Provider API validation script.

Tests Z.AI and MiniMax modern APIs to understand their actual response format.
This helps ensure transformer updates match real API behavior.
"""

import os
import json
import sys
from typing import Any

# Optional: use OpenAI client for easier testing
try:
    from openai import OpenAI
    HAS_OPENAI = True
except ImportError:
    HAS_OPENAI = False
    print("Warning: openai package not installed. Using requests only.")
    import requests

def test_zai_api() -> dict[str, Any]:
    """Test Z.AI GLM-5.2 API response format."""
    api_key = os.environ.get("ZAI_API_KEY")
    if not api_key:
        print("SKIP: ZAI_API_KEY not set")
        return {"status": "skipped"}

    print("\n=== Testing Z.AI GLM-5.2 ===")

    if HAS_OPENAI:
        client = OpenAI(
            api_key=api_key,
            base_url="https://api.z.ai/api/coding/paas/v4"
        )

        # Test 1: Basic request with reasoning_effort
        print("\n1. Testing with reasoning_effort parameter...")
        try:
            response = client.chat.completions.create(
                model="glm-5.2",
                messages=[
                    {"role": "system", "content": "You are a helpful assistant."},
                    {"role": "user", "content": "What is 2+2? Think step by step."}
                ],
                max_tokens=500,
                reasoning_effort="high",  # New parameter for GLM-5.2
                stream=False
            )
            print(f"Response structure: {response.model_dump(exclude={'choices'})}")
            if response.choices:
                print(f"First choice keys: {response.choices[0].model_dump(exclude={'message'}).keys()}")
                msg = response.choices[0].message
                print(f"Message keys: {msg.model_dump(exclude={'content'}).keys() if hasattr(msg, 'model_dump') else dir(msg)}")
                print(f"Content preview: {msg.content[:200] if msg.content else 'None'}...")
                if hasattr(msg, 'reasoning_content'):
                    print(f"Reasoning: {msg.reasoning_content[:100] if msg.reasoning_content else 'None'}...")
        except Exception as e:
            print(f"Error with reasoning_effort: {e}")

        # Test 2: Check if thinking parameter works
        print("\n2. Testing with thinking parameter...")
        try:
            response = client.chat.completions.create(
                model="glm-5.1",
                messages=[
                    {"role": "user", "content": "What is 2+2? Think step by step."}
                ],
                max_tokens=500,
                thinking={"type": "enabled"},
                stream=False
            )
            msg = response.choices[0].message
            print(f"Content: {msg.content[:200] if msg.content else 'None'}...")
            if hasattr(msg, 'reasoning_content'):
                print(f"Reasoning present: {bool(msg.reasoning_content)}")
        except Exception as e:
            print(f"Error with thinking parameter: {e}")

        # Test 3: Streaming check
        print("\n3. Testing streaming response...")
        try:
            stream = client.chat.completions.create(
                model="glm-5.2",
                messages=[{"role": "user", "content": "Say 'hello'"}],
                max_tokens=50,
                stream=True
            )
            chunks = []
            for chunk in stream:
                chunks.append(chunk)
                if len(chunks) >= 5:
                    break
            print(f"First chunk keys: {chunks[0].model_dump(exclude={'choices'}).keys()}")
            if chunks[0].choices:
                delta = chunks[0].choices[0].delta
                print(f"Delta keys: {delta.model_dump(exclude={'content'}).keys() if hasattr(delta, 'model_dump') else [k for k in dir(delta) if not k.startswith('_')]}")
        except Exception as e:
            print(f"Error with streaming: {e}")

    return {"status": "tested"}

def test_minimax_api() -> dict[str, Any]:
    """Test MiniMax M3 API response format."""
    api_key = os.environ.get("MINIMAX_API_KEY")
    if not api_key:
        print("SKIP: MINIMAX_API_KEY not set")
        return {"status": "skipped"}

    print("\n=== Testing MiniMax M3 ===")

    if HAS_OPENAI:
        # Test with OpenAI-compatible endpoint
        print("\n1. Testing OpenAI-compatible endpoint...")
        client = OpenAI(
            api_key=api_key,
            base_url="https://api.minimax.io/v1"
        )

        try:
            response = client.chat.completions.create(
                model="MiniMax-M3",
                messages=[
                    {"role": "system", "content": "You are a helpful assistant."},
                    {"role": "user", "content": "What is 2+2? Think step by step."}
                ],
                max_tokens=500,
                extra_body={"reasoning_split": True},  # Current transformer uses this
                stream=False
            )
            msg = response.choices[0].message
            print(f"Content preview: {msg.content[:200] if msg.content else 'None'}...")
            if hasattr(msg, 'reasoning_details') or hasattr(msg, 'reasoning_content'):
                reasoning = getattr(msg, 'reasoning_details', None) or getattr(msg, 'reasoning_content', None)
                print(f"Reasoning present: {bool(reasoning)}")
                if reasoning:
                    print(f"Reasoning preview: {reasoning[:100] if len(str(reasoning)) > 100 else reasoning}...")
        except Exception as e:
            print(f"Error with OpenAI endpoint: {e}")

        # Test 2: Try Anthropic-compatible endpoint
        print("\n2. Testing Anthropic-compatible endpoint...")
        try:
            import anthropic
            a_client = anthropic.Anthropic(
                api_key=api_key,
                base_url="https://api.minimax.io/anthropic"
            )

            response = a_client.messages.create(
                model="MiniMax-M3",
                max_tokens=500,
                messages=[
                    {"role": "user", "content": "What is 2+2? Think step by step."}
                ],
                thinking={"type": "adaptive"},  # M3 uses adaptive thinking
            )

            print(f"Response type: {response.type}")
            print(f"Number of content blocks: {len(response.content)}")
            for i, block in enumerate(response.content):
                print(f"Block {i}: type={block.type}, ", end="")
                if block.type == "thinking":
                    print(f"thinking length={len(block.thinking)}")
                elif block.type == "text":
                    print(f"text preview={block.text[:50]}...")
                else:
                    print(f"other")
        except ImportError:
            print("SKIP: anthropic package not installed")
        except Exception as e:
            print(f"Error with Anthropic endpoint: {e}")

        # Test 3: Check M2.7 behavior for comparison
        print("\n3. Testing M2.7 for comparison...")
        try:
            response = client.chat.completions.create(
                model="MiniMax-M2.7",
                messages=[{"role": "user", "content": "What is 2+2?"}],
                max_tokens=200,
                extra_body={"reasoning_split": True},
                stream=False
            )
            msg = response.choices[0].message
            print(f"M2.7 Content: {msg.content[:200] if msg.content else 'None'}...")
        except Exception as e:
            print(f"Error with M2.7: {e}")

    return {"status": "tested"}

def main():
    print("Provider API Validation Script")
    print("=" * 50)

    results = {
        "zai": test_zai_api(),
        "minimax": test_minimax_api()
    }

    print("\n=== Summary ===")
    print(f"Z.AI: {results['zai']['status']}")
    print(f"MiniMax: {results['minimax']['status']}")

if __name__ == "__main__":
    main()
