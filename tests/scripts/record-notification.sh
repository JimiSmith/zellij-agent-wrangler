#!/bin/sh
# Stand in for the desktop notifier: write down what was raised, one line per
# notification, and say nothing on screen.
#
# The path is derived from this script's own location rather than from the
# working directory, because whatever runs it was not started from anywhere in
# particular.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$root/tests/out"
printf '%s | %s\n' "${1:-}" "${2:-}" >>"$root/tests/out/raised.log"
