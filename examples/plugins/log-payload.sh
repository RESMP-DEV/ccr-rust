#!/bin/sh
# Pass-through logger: appends one JSON line per invocation to
# $CCR_PLUGIN_LOG (default /tmp/ccr-plugin.log), then forwards the payload
# unchanged on stdout.
#
# Demonstrates the CCR_STAGE and CCR_PLUGIN_NAME environment variables and
# the pass-through contract. Stages: both. Requires jq. See docs/plugins.md.
tmp=$(mktemp) || exit 1
trap 'rm -f "$tmp"' EXIT
cat > "$tmp"
jq -c --arg stage "$CCR_STAGE" --arg plugin "$CCR_PLUGIN_NAME" \
    --slurpfile payload "$tmp" \
    '{stage: $stage, plugin: $plugin, payload: $payload[0]}' \
    >> "${CCR_PLUGIN_LOG:-/tmp/ccr-plugin.log}" < "$tmp"
cat "$tmp"
