#!/bin/sh
# Run this script through sh so the hook finds the long-lived test harness.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
USER=wrangler-test
XDG_STATE_HOME="$root/tests/out/state"
export USER XDG_STATE_HOME
session=$1
pane=$2
event=$3
server=$(tmux -L wrangler-test display-message -p -t "$pane" '#{socket_path},#{pid},#{session_id}')
printf '%s' "{\"session_id\":\"$session\",\"cwd\":\"/home/u/quarry\",\"transcript_path\":\"$root/tests/tmux-transcript.jsonl\"}" |
    TMUX="$server" TMUX_PANE="$pane" "$root/target/debug/agent-wrangler" hook claude "$event"
