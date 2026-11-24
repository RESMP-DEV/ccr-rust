#!/bin/sh
# Pass-through logger: appends one JSON line per invocation to the plugin log,
# then forwards the payload unchanged on stdout.
#
# The log holds complete request and response payloads, so it defaults to an
# owner-only file in a private state directory, is created with mode 0600, and
# refuses to append to a symlink or a file owned by another user. Override the
# destination with CCR_PLUGIN_LOG; the same checks apply.
#
# Demonstrates the CCR_STAGE and CCR_PLUGIN_NAME environment variables and
# the pass-through contract. Stages: both. Requires jq. See docs/plugins.md.
set -eu
umask 077

log_file="${CCR_PLUGIN_LOG:-${XDG_STATE_HOME:-$HOME/.local/state}/ccr-rust/plugin-payload.jsonl}"
log_dir=$(dirname "$log_file")
mkdir -p "$log_dir"
chmod 700 "$log_dir" 2>/dev/null || true

if [ -L "$log_file" ]; then
    echo "refusing to append to symlinked plugin log: $log_file" >&2
    exit 1
fi
if [ -e "$log_file" ]; then
    log_owner=$(stat -f '%u' "$log_file" 2>/dev/null || stat -c '%u' "$log_file" 2>/dev/null || echo "")
    if [ -n "$log_owner" ] && [ "$log_owner" != "$(id -u)" ]; then
        echo "refusing to append to plugin log owned by another user: $log_file" >&2
        exit 1
    fi
    chmod 600 "$log_file" 2>/dev/null || true
fi

tmp=$(mktemp) || exit 1
trap 'rm -f "$tmp"' EXIT
cat > "$tmp"
jq -c --arg stage "$CCR_STAGE" --arg plugin "$CCR_PLUGIN_NAME" \
    --slurpfile payload "$tmp" \
    '{stage: $stage, plugin: $plugin, payload: $payload[0]}' \
    >> "$log_file"
cat "$tmp"
