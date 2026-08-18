#!/bin/sh
# This script sends one named pipe message into a session from outside it.
#
# The script sets the cache directory itself, and does not inherit it. Zellij
# finds a live session through that directory. If a command points at a
# different cache directory, the command reports no such session while the
# session still runs.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
XDG_CACHE_HOME="$root/tests/out/cache"
export XDG_CACHE_HOME
zellij --session "$1" pipe --name "$2" -- "$3" </dev/null
