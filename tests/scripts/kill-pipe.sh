#!/bin/sh
# This script counts the pipes that the daemon holds open to one test session,
# and then kills them.
#
# The count is an assertion of its own. The daemon holds one pipe per session
# and writes one line per publish. A daemon that ran a pipe per delivery instead
# has none alive between two publishes. It has several alive at once during
# one.
#
# The script refuses any name that a test run did not create. Nothing here can
# reach a session of the developer.
set -eu
session=$1
case "$session" in
wrangler-test-*) ;;
*)
    echo "refusing to reach $session" >&2
    exit 1
    ;;
esac

held=$(ps -e -o pid=,args= | grep -F -- "--session $session pipe" | grep -v grep || true)
count=$(printf '%s' "$held" | grep -c . || true)
if [ "$count" != "1" ]; then
    echo "the daemon holds $count pipes for $session, wanted 1" >&2
    printf '%s\n' "$held" >&2
    exit 1
fi

printf '%s\n' "$held" | awk '{print $1}' | xargs kill
echo "killed the one held pipe for $session"
