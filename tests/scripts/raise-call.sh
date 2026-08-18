#!/bin/sh
# This script reports one event to the daemon in the same way as an agent in a
# named zellij pane. The script needs no pane.
#
# The script sets the location variables itself, and does not inherit them. The
# hook reads these variables and nothing else to say where the event came from.
# If a hook runs on the host with none of these variables set, the hook reports
# from nowhere and the daemon drops it.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
session=$1
zellij_session=$2
pane=$3
event=$4

printf '%s' "{\"session_id\":\"$session\",\"cwd\":\"/home/u/quarry\",\"transcript_path\":\"$root/tests/out/e2e-transcript.jsonl\"}" |
    ZELLIJ=0 ZELLIJ_SESSION_NAME="$zellij_session" ZELLIJ_PANE_ID="$pane" \
        "$root/target/debug/agent-wrangler" hook claude "$event"
