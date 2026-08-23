#!/bin/sh
# This script makes sure that a zellij sidebar tells the daemon that it is
# there.
#
# The daemon gives up on a client that says nothing. A sidebar draws the same
# either way for ninety seconds, so the screen cannot show this. The daemon's
# own monitor stream can, and that is what this script reads.
#
# A plugin writes on a pipe only while it handles a message from that pipe. So
# this script reports an agent, which makes the daemon publish, which is the
# message. The line that comes back is the beat.
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
# The agent names itself for this run alone. The daemon keeps its state across a
# restart, so a name that an earlier run used changes nothing and makes the
# daemon publish nothing.
probe="beat-probe-$$"
records="$root/tests/out/beat-monitor.txt"
: >"$records"

"$root/target/debug/agent-wrangler" monitor >"$records" 2>&1 &
monitor=$!
# The monitor asks for everything from now on. A record raised before the daemon
# read that request reaches nobody.
sleep 1

printf '%s' "{\"session_id\":\"$probe\",\"cwd\":\"/home/u/quarry\",\"transcript_path\":\"$root/tests/out/e2e-transcript.jsonl\"}" |
    "$root/target/debug/agent-wrangler" hook claude start

sleep 5
kill "$monitor" 2>/dev/null || true

if grep -q "\"kind\":\"beat\".*\"session\":\"$want_session\"" "$records"; then
    echo "$want_session says that it is there"
    exit 0
fi
echo "no beat from $want_session" >&2
cat "$records" >&2
exit 1
