#!/bin/sh
# Stands in for the client so a run can see what the sidebar actually asked to
# be run, then does the real thing so the rest of the path still works.
root=$(cd "$(dirname "$0")/../.." && pwd)
printf '%s\n' "$*" >>"$root/tests/out/ran.txt"
exec "$root/target/debug/agent-wrangler" "$@"
