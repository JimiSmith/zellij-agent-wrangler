#!/bin/sh
# This script writes everything that the end to end run needs before zellij
# starts:
# - the layout, with the built wasm and the built client put in place
# - the transcript that the test session reads its own title from
# - the permission cache
#
# The script answers the permissions in advance. To ask for permission, zellij
# draws over the pane of the plugin. Zellij draws no plugin at all while the
# request waits. If a run does not answer the permissions in advance, the run
# asserts on a sidebar that has no permission to read anything.
#
# The notifier stays off unless the caller names one. If a run raises a real
# desktop notification, the run interrupts the person who runs the tests.
#
# Every template gets the same notifier. The sidebars of a session are one
# client, and the last sidebar to register sets what that client wants. If the
# templates of a layout disagree, the order of the tabs at open time settles the
# result.
#
# The layout comes from `tests/tree.kdl` unless the caller names another one.
# The harness owns that file, so that a change to `dev.kdl` cannot move what a
# run asserts on. A run that needs another layout names it.
set -eu
out=$1
notifier=${2:-off}
source_layout=${3:-tests/tree.kdl}
root=$(cd "$(dirname "$0")/../.." && pwd)
wasm="$root/target/wasm32-wasip1/debug/zellij-agent-wrangler.wasm"

mkdir -p "$(dirname "$out")" "$root/tests/out/cache/zellij"

sed -e "s#PLUGIN_LOCATION#file:$wasm#" \
    -e "s#CLIENT_LOCATION#$root/tests/scripts/wrangler-probe.sh#" \
    -e "s#desktop_notification \"[^\"]*\"#desktop_notification \"$notifier\"#" \
    "$root/$source_layout" >"$out"

printf '%s\n' '{"type":"custom-title","customTitle":"QUARRYMARK"}' \
    >"$root/tests/out/e2e-transcript.jsonl"

# The two files of one child of that session. Claude keeps them under a
# directory named for the lead session, beside the lead's own transcript, and
# the daemon builds both paths from the transcript that a hook names.
children="$root/tests/out/e2e-transcript/subagents"
mkdir -p "$children"
printf '%s\n' '{"agentType":"Explore","description":"read the quarry","color":"purple"}' \
    >"$children/agent-probe-one.meta.json"
printf '%s\n' '{"type":"user","message":"hello"}' \
    >"$children/agent-probe-one.jsonl"

# A child of that child. Claude writes parentAgentId exactly when the parent is
# not the session, so this one file is what puts the row two levels deep.
printf '%s\n' \
    '{"agentType":"Plan","description":"plan the quarry","parentAgentId":"probe-one"}' \
    >"$children/agent-probe-two.meta.json"
printf '%s\n' '{"type":"user","message":"hello"}' \
    >"$children/agent-probe-two.jsonl"

cat >"$root/tests/out/cache/zellij/permissions.kdl" <<KDL
"$wasm" {
    ReadApplicationState
    ChangeApplicationState
    MessageAndLaunchOtherPlugins
    RunCommands
    ReadCliPipes
}
KDL
