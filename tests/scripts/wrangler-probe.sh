#!/bin/sh
# This script stands in for the client, so that a run can see what the sidebar
# asked for. The script then runs the real client, so that the rest of the path
# still works.
root=$(cd "$(dirname "$0")/../.." && pwd)
printf '%s\n' "$*" >>"$root/tests/out/ran.txt"
exec "$root/target/debug/agent-wrangler" "$@"
