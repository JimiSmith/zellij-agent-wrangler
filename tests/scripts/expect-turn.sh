#!/bin/sh
# Assert that the daemon holds one session in a given turn state.
#
# Reading it back from the daemon rather than from the screen is what makes this
# an assertion about the reverse channel: the sidebar is what noticed the user
# was already there, and the daemon only knows because the sidebar told it.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
XDG_STATE_HOME="$root/tests/out/state"
export XDG_STATE_HOME
want_session=$1
want_turn=$2
line=$("$root/target/debug/agent-wrangler" agents | awk -F'\t' -v s="$want_session" '$2==s')
if [ -z "$line" ]; then
    echo "no record for $want_session" >&2
    "$root/target/debug/agent-wrangler" agents >&2
    exit 1
fi
turn=$(printf '%s' "$line" | cut -f5)
if [ "$turn" != "$want_turn" ]; then
    echo "$want_session is $turn, wanted $want_turn" >&2
    exit 1
fi
echo "$want_session is $want_turn"
