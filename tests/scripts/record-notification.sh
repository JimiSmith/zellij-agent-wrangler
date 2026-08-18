#!/bin/sh
# This script stands in for the desktop notifier. It writes down what the run
# raised, one line for each notification, and puts nothing on the screen.
#
# The script derives the path from its own location, not from the working
# directory. The program that runs this script starts from no fixed directory.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$root/tests/out"
printf '%s | %s\n' "${1:-}" "${2:-}" >>"$root/tests/out/raised.log"
