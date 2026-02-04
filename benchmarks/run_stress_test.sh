#!/usr/bin/env bash
# run_stress_test.sh - End-to-end stress test orchestrator
#
# Starts the mock backend, starts ccr-rust, runs the stress test, and
# tears everything down. All processes are killed on exit (including Ctrl-C).
#
# Usage:
#   cd contrib/ccr-rust
#   ./benchmarks/run_stress_test.sh [--streams 100] [--chunks 20] [--delay-ms 50]
#
# Requirements:
#   - ccr-rust binary built: cargo build --release
#   - Python deps: uv pip install aiohttp uvicorn starlette

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$PROJECT_DIR/../.." && pwd)"

# Defaults
STREAMS=100
CHUNKS=20
DELAY_MS=50
RAMP_MS=0
MOCK_PORT=9999
CCR_PORT=3456
JSON_OUT=""

# Parse arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --streams)   STREAMS="$2"; shift 2 ;;
        --chunks)    CHUNKS="$2"; shift 2 ;;
        --delay-ms)  DELAY_MS="$2"; shift 2 ;;
        --ramp-ms)   RAMP_MS="$2"; shift 2 ;;
        --mock-port) MOCK_PORT="$2"; shift 2 ;;
        --ccr-port)  CCR_PORT="$2"; shift 2 ;;
        --json-out)  JSON_OUT="$2"; shift 2 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

# PIDs to clean up
PIDS=()

cleanup() {
    echo ""
    echo "Cleaning up..."
    for pid in "${PIDS[@]}"; do
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
        fi
    done
    echo "Done."
}

trap cleanup EXIT INT TERM

# Locate ccr-rust binary
CCR_BIN="$PROJECT_DIR/target/release/ccr-rust"
if [[ ! -x "$CCR_BIN" ]]; then
    echo "ccr-rust binary not found at $CCR_BIN"
    echo "Building with: cargo build --release"
    (cd "$PROJECT_DIR" && cargo build --release)
fi

# Generate config pointing at mock backend on the right port
CONFIG_FILE=$(mktemp /tmp/ccr-bench-config-XXXXXX.json)
cat > "$CONFIG_FILE" << EOF
{
    "PORT": $CCR_PORT,
    "HOST": "127.0.0.1",
    "API_TIMEOUT_MS": 120000,
    "Providers": [
        {
            "name": "mock",
            "api_base_url": "http://127.0.0.1:$MOCK_PORT",
            "api_key": "mock-key",
            "models": ["mock-model"]
        }
    ],
    "Router": {
        "default": "mock,mock-model"
    }
}
EOF

echo "============================================================"
echo "  CCR-RUST SSE STRESS TEST"
echo "============================================================"
echo "  Streams:    $STREAMS"
echo "  Chunks:     $CHUNKS per stream"
echo "  Delay:      ${DELAY_MS}ms between chunks"
echo "  Ramp:       ${RAMP_MS}ms"
echo "  Mock port:  $MOCK_PORT"
echo "  CCR port:   $CCR_PORT"
echo "============================================================"
echo ""

# 1. Start mock backend
echo "[1/3] Starting mock SSE backend on port $MOCK_PORT..."
(cd "$REPO_ROOT" && uv run python "$SCRIPT_DIR/mock_sse_backend.py" \
    --port "$MOCK_PORT" --chunks "$CHUNKS" --delay-ms "$DELAY_MS") &
PIDS+=($!)
sleep 1

# Verify mock is up
if ! curl -sf "http://127.0.0.1:$MOCK_PORT/health" > /dev/null 2>&1; then
    echo "ERROR: Mock backend failed to start"
    exit 1
fi
echo "  Mock backend ready."

# 2. Start ccr-rust
echo "[2/3] Starting ccr-rust on port $CCR_PORT..."
"$CCR_BIN" --config "$CONFIG_FILE" --port "$CCR_PORT" &
PIDS+=($!)
sleep 1

# Verify ccr-rust is up
if ! curl -sf "http://127.0.0.1:$CCR_PORT/health" > /dev/null 2>&1; then
    echo "ERROR: ccr-rust failed to start"
    exit 1
fi
echo "  ccr-rust ready."

# 3. Run stress test
echo "[3/3] Running stress test..."
echo ""

JSON_ARGS=""
if [[ -n "$JSON_OUT" ]]; then
    JSON_ARGS="--json-out $JSON_OUT"
fi

(cd "$REPO_ROOT" && uv run python "$SCRIPT_DIR/stress_sse_streams.py" \
    --ccr-url "http://127.0.0.1:$CCR_PORT" \
    --streams "$STREAMS" \
    --ramp-ms "$RAMP_MS" \
    --model "mock,mock-model" \
    $JSON_ARGS)

EXIT_CODE=$?

# Clean up temp config
rm -f "$CONFIG_FILE"

exit $EXIT_CODE
