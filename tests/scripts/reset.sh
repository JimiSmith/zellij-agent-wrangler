#!/bin/sh
# This script clears what an earlier run left behind: the daemon that the run
# started, and the record that zellij keeps of the test session.
#
# The script sets the cache directory itself, and does not inherit it. Zellij
# keeps a session that it can resurrect under that directory. If a delete points
# at a different cache directory, the delete succeeds without a message. The
# dead session then remains and stops the next run.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
XDG_CACHE_HOME="$root/tests/out/cache"
export XDG_CACHE_HOME

ps -e -o pid=,args= | grep -F "$root/target/debug/agent-wrangler daemon" | grep -v grep |
    awk '{print $1}' | xargs -r kill 2>/dev/null || true

zellij delete-session "${1:-wrangler-test-e2e}" --force >/dev/null 2>&1 || true
exit 0
