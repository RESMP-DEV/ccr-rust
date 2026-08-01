#!/usr/bin/env bash
# Smoke-test a running CCR-Rust router.
#
# Usage:
#   ./examples/smoke-test.sh                     # defaults: 127.0.0.1:3456, model "deepseek-chat"
#   CCR_HOST=10.0.0.5 CCR_PORT=8080 ./examples/smoke-test.sh
#   MODEL="glm-5.2" ./examples/smoke-test.sh
#
# Requires: curl. The router must already be running (ccr-rust start).

set -euo pipefail

HOST="${CCR_HOST:-127.0.0.1}"
PORT="${CCR_PORT:-3456}"
BASE="http://${HOST}:${PORT}"
MODEL="${MODEL:-deepseek-chat}"

echo "==> Health check (${BASE}/health)"
curl -fsS "${BASE}/health"
echo

echo "==> Configured models (${BASE}/v1/models)"
curl -fsS "${BASE}/v1/models"
echo

echo "==> Anthropic-style chat request (model: ${MODEL})"
curl -fsS "${BASE}/v1/messages" \
  -H "content-type: application/json" \
  -d "{
    \"model\": \"${MODEL}\",
    \"max_tokens\": 128,
    \"messages\": [{\"role\": \"user\", \"content\": \"Reply with exactly: ok\"}]
  }"
echo

echo "==> OpenAI-style chat request (model: ${MODEL})"
curl -fsS "${BASE}/v1/chat/completions" \
  -H "content-type: application/json" \
  -d "{
    \"model\": \"${MODEL}\",
    \"max_tokens\": 128,
    \"messages\": [{\"role\": \"user\", \"content\": \"Reply with exactly: ok\"}]
  }"
echo

echo "==> Done."
