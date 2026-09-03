#!/bin/sh
# This script makes sure that the daemon files one agent under the session that
# started it.
#
# The lead is field 17 of the encoded record, and the title is the remainder of
# the line after it. A hook that fires inside a child names the lead in
# session_id and names the child in agent_id, so the daemon files the child
# under an id composed of the two.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
XDG_STATE_HOME="$root/tests/out/state"
export XDG_STATE_HOME
# The socket of the daemon is named for the user and for nothing else. A
# developer of this project has the real client installed, and its daemon holds
# that name. Without a name of its own, a run reports to that daemon and asserts
# on it. The build under test is never exercised at all.
USER=wrangler-test
export USER
want_session=$1
want_lead=$2
line=$("$root/target/debug/agent-wrangler" agents | awk -F'\t' -v s="$want_session" '$2==s')
if [ -z "$line" ]; then
    echo "no record for $want_session" >&2
    "$root/target/debug/agent-wrangler" agents >&2
    exit 1
fi
lead=$(printf '%s' "$line" | cut -f17)
if [ "$lead" != "$want_lead" ]; then
    echo "$want_session names lead '$lead', wanted '$want_lead'" >&2
    exit 1
fi
echo "$want_session names lead $want_lead"
