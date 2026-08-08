#!/bin/sh
# Assert that exactly this many desktop notifications were raised.
#
# Counting rather than looking for one is the whole point: a call announced by
# each sidebar holding it would still put the right words on screen, and would
# put them there once per sidebar.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
log="$root/tests/out/raised.log"
want=$1
raised=0
[ -f "$log" ] && raised=$(wc -l <"$log")
if [ "$raised" -ne "$want" ]; then
    echo "$raised notifications were raised, wanted $want" >&2
    cat "$log" >&2 2>/dev/null || true
    exit 1
fi
echo "$raised notifications raised, as wanted"
