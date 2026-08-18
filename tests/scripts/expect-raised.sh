#!/bin/sh
# This script makes sure that the run raised exactly this many desktop
# notifications.
#
# The count is the point, not the presence of one notification. If each sidebar
# that holds a call announces that call, the screen still shows the right words.
# The notifier then runs one time for each sidebar.
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
