#!/bin/sh
# Strip Anthropic cache_control markers from protocol metadata blocks.
#
# Some OpenAI-compatible upstreams reject unknown fields on otherwise valid
# payloads. This plugin removes cache_control from typed content blocks and the
# system field, while leaving application-owned data alone: tool arguments
# (`input`), tool schemas (`input_schema`, `parameters`, `properties`), and any
# other nested value are forwarded unchanged.
#
# Stages: both. Requires jq. See docs/plugins.md.
exec jq '
  def strip_blocks:
    if type == "object" then
      (if (has("type") and (.type | type) == "string") then del(.cache_control) else . end)
      | with_entries(
          if (.key | IN("input", "input_schema", "parameters", "properties")) then .
          else .value |= strip_blocks
          end)
    elif type == "array" then map(strip_blocks)
    else . end;
  strip_blocks'
