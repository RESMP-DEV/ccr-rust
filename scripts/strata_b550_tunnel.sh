#!/usr/bin/env bash
# Keep CCR's local Strata forward available across transient B550 paths.
# ZeroTier is first; the router-resolved LAN and Strix jump aliases remain
# authenticated fallbacks. No path uses a historical IPv4 address as identity.

set -u

hosts=(b550-zt B550 b550-jump)
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
        printf '[strata-b550-tunnel] trying %s\n' "$host"
        /usr/bin/ssh -N \
            -L 127.0.0.1:18081:127.0.0.1:18080 \
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
        printf '[strata-b550-tunnel] %s ended with status %s\n' "$host" "$status"
        if [ "$status" -eq 0 ]; then
            break
        fi
    done
    sleep 10
done
