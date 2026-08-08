#!/bin/sh
# Send one named pipe message into a session from outside it.
#
# The cache directory is set here rather than inherited, because zellij finds a
# running session through it and a command aimed at a different one reports no
# such session while the session is plainly running.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
XDG_CACHE_HOME="$root/tests/out/cache"
export XDG_CACHE_HOME
zellij --session "$1" pipe --name "$2" -- "$3" </dev/null
