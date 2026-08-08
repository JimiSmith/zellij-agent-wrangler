#!/bin/sh
# Clear what an earlier run left behind: the daemon it started, and zellij's
# record of the test session.
#
# The cache directory is set here rather than inherited, because zellij keeps a
# session it can resurrect under that directory and a delete aimed at a
# different one silently succeeds while leaving the corpse that stops the next
# run from starting.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
XDG_CACHE_HOME="$root/tests/out/cache"
export XDG_CACHE_HOME

ps -e -o pid=,args= | grep -F "$root/target/debug/agent-wrangler daemon" | grep -v grep |
    awk '{print $1}' | xargs -r kill 2>/dev/null || true

zellij delete-session "${1:-wrangler-test-e2e}" --force >/dev/null 2>&1 || true
exit 0
