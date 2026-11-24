#!/usr/bin/env bash
# Keep CCR's local Tetra vLLM forward available across transient SSH paths.
# The only authenticated route is the named ZeroTier alias; CCR never targets
# Tetra's overlay address directly.

set -u

hosts=(tetra)
ssh_pid=""

cleanup() {
    if [ -n "$ssh_pid" ]; then
        kill "$ssh_pid" 2>/dev/null || true
    fi
    exit 0
}
trap cleanup TERM INT HUP

while true; do
    for host in "${hosts[@]}"; do
        printf '[tetra-vllm-tunnel] trying %s\n' "$host"
        /usr/bin/ssh -N \
            -L 127.0.0.1:18317:127.0.0.1:8317 \
            -o ExitOnForwardFailure=yes \
            -o ServerAliveInterval=10 \
            -o ServerAliveCountMax=3 \
            -o BatchMode=yes \
            -o ControlMaster=no \
            -o ControlPersist=no \
            "$host" &
        ssh_pid=$!
        wait "$ssh_pid"
        status=$?
        ssh_pid=""
        printf '[tetra-vllm-tunnel] %s ended with status %s\n' "$host" "$status"
        if [ "$status" -eq 0 ]; then
            break
        fi
    done
    sleep 10
done
