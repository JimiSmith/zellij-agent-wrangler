#!/bin/sh
# This script clears what an earlier tmux run left behind: the tmux server that
# the harness starts, and the daemon that the run started.
#
# The server name is written here and is never taken from an argument. A server
# name from outside would let this script end the server of the developer.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)

tmux -L wrangler-test kill-server >/dev/null 2>&1 || true

ps -e -o pid=,args= | grep -F "$root/target/debug/agent-wrangler daemon" | grep -v grep |
    awk '{print $1}' | xargs -r kill 2>/dev/null || true

exit 0
