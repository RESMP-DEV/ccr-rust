#!/bin/sh
# Strip Anthropic cache_control markers from every block of the payload.
#
# Some OpenAI-compatible upstreams reject unknown fields on otherwise valid
# payloads; this plugin removes cache_control recursively from objects,
# arrays, and the system field, then forwards the result.
#
# Stages: both. Requires jq. See docs/plugins.md.
exec jq 'walk(if type == "object" and has("cache_control") then del(.cache_control) else . end)'
