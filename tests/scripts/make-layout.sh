#!/bin/sh
# Write everything the end to end run needs before zellij starts: the layout
# with the built wasm and the built client substituted in, the transcript the
# test session reads its own title from, and the permission cache.
#
# The permissions are pre-answered because zellij asks by drawing over the
# plugin's pane and does not render a plugin at all while its request is
# pending, so a run that did not answer them in advance would be asserting on a
# sidebar that has not been allowed to read anything.
set -eu
out=$1
root=$(cd "$(dirname "$0")/../.." && pwd)
wasm="$root/target/wasm32-wasip1/debug/zellij-agent-wrangler.wasm"

mkdir -p "$(dirname "$out")" "$root/tests/out/cache/zellij"

sed -e "s#PLUGIN_LOCATION#file:$wasm#" \
    -e "s#CLIENT_LOCATION#$root/tests/scripts/wrangler-probe.sh#" \
    -e "s#desktop_notification \"on\"#desktop_notification \"off\"#" \
    "$root/dev.kdl" >"$out"

printf '%s\n' '{"type":"custom-title","customTitle":"QUARRYMARK"}' \
    >"$root/tests/out/e2e-transcript.jsonl"

cat >"$root/tests/out/cache/zellij/permissions.kdl" <<KDL
"$wasm" {
    ReadApplicationState
    ChangeApplicationState
    MessageAndLaunchOtherPlugins
    RunCommands
}
KDL
