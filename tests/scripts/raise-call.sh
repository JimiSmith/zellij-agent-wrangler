#!/bin/sh
# Report one event to the daemon as an agent running in a named zellij pane
# would, without there having to be a pane.
#
# The location variables are set here rather than inherited, because they are
# the whole of what the hook reads to say where it was raised: a hook run on the
# host with none of them set reports from nowhere and is dropped.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
session=$1
zellij_session=$2
pane=$3
event=$4

printf '%s' "{\"session_id\":\"$session\",\"cwd\":\"/home/u/quarry\",\"transcript_path\":\"$root/tests/out/e2e-transcript.jsonl\"}" |
    ZELLIJ=0 ZELLIJ_SESSION_NAME="$zellij_session" ZELLIJ_PANE_ID="$pane" \
        "$root/target/debug/agent-wrangler" hook claude "$event"
