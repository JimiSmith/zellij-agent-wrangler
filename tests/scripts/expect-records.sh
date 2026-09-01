#!/bin/sh
# This script makes sure that the daemon holds one of the two transcript records
# for a session, and that the record holds what the run expects.
#
# The record is read back from the daemon rather than from the screen. Nothing
# draws these records yet.
#
# The fields of a record are counted from one: 15 is the last message and 16 is
# the tool that runs. A caller names `message` or `tool` rather than a number.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
XDG_STATE_HOME="$root/tests/out/state"
export XDG_STATE_HOME
# The socket of the daemon is named for the user. Without a name of its own, a
# run reports to the daemon that the developer installed.
USER=wrangler-test
export USER
want_session=$1
which_record=$2
want=${3:-}
case "$which_record" in
    message) column=15 ;;
    tool) column=16 ;;
    *) echo "no record called $which_record" >&2; exit 1 ;;
esac
line=$("$root/target/debug/agent-wrangler" agents | awk -F'\t' -v s="$want_session" '$2==s')
if [ -z "$line" ]; then
    echo "no record for $want_session" >&2
    exit 1
fi
held=$(printf '%s' "$line" | cut -f"$column")
if [ -z "$want" ]; then
    if [ -n "$held" ]; then
        echo "$which_record holds $(printf '%s' "$held" | wc -c) bytes, wanted nothing" >&2
        exit 1
    fi
    echo "$which_record is empty, as wanted"
    exit 0
fi
case "$held" in
    *"$want"*) echo "$which_record holds $want, in $(printf '%s' "$held" | wc -c) bytes" ;;
    *) echo "$which_record does not hold $want" >&2
       printf '%s\n' "$held" | head -c 300 >&2
       exit 1 ;;
esac
