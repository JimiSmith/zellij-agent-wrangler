#!/bin/sh
# This script appends the result that answers one tool call, so that the run can
# assert that the call stops being reported.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
id=$1
printf '%s\n' \
    "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"$id\",\"content\":\"written\"}]}}" \
    >>"$root/tests/out/e2e-transcript.jsonl"
echo "appended the result that answers $id"
