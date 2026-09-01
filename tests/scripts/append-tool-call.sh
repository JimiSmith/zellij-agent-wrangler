#!/bin/sh
# This script appends one tool call to the transcript that the end to end run
# reads.
#
# The input is padded to a size that the caller names. A tool call that carries
# a file runs to tens of kilobytes, and that record travels whole to every
# client. Nothing else in the harness puts a payload of that size through
# `zellij pipe`.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
id=$1
bytes=$2
padding=$(head -c "$bytes" /dev/zero | tr '\0' 'x')
printf '%s\n' \
    "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-5\",\"content\":[{\"type\":\"tool_use\",\"id\":\"$id\",\"name\":\"Write\",\"input\":{\"content\":\"$padding\"}}]}}" \
    >>"$root/tests/out/e2e-transcript.jsonl"
echo "appended a tool call of $bytes bytes as $id"
