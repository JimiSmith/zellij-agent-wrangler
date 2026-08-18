#!/usr/bin/env bash
# Build the plugin and open a zellij session with the plugin in every tab.
#
# This script kills an existing `wrangler-proto` session. Everything else in
# that session stops with it. A session loads the wasm once and holds it, so an
# attach runs the build before the last one, and it gives no message.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# The build of the plugin is optimized. A session with many tabs draws a frame
# on every event. An unoptimized build makes that work ten times larger. The
# prototype is then slow enough to look like a prototype that draws the wrong
# thing.
wasm="$root/target/wasm32-wasip1/dev-wasm/zellij-agent-wrangler.wasm"

cargo build --manifest-path "$root/Cargo.toml" --target wasm32-wasip1 \
    --profile dev-wasm -p zellij-agent-wrangler
cargo build --manifest-path "$root/Cargo.toml" -p agent-wrangler

client="$root/target/debug/agent-wrangler"
echo "client: $client"

# A daemon that already runs comes from an earlier build, and every hook and
# every sidebar reaches that daemon. This command stops it. The next event then
# starts the daemon from this build.
ps -e -o pid=,args= | grep -F "$client daemon" | grep -v grep |
    awk '{print $1}' | xargs -r kill 2>/dev/null || true

layout="$(mktemp -d)/dev.kdl"
sed -e "s#PLUGIN_LOCATION#file:$wasm#" \
    -e "s#CLIENT_LOCATION#$client#" "$root/dev.kdl" >"$layout"

zellij delete-session wrangler-proto --force >/dev/null 2>&1 || true

# `--session` with `--layout` attaches and creates nothing. The layout
# therefore comes in on the flag that always starts a new session.
exec zellij --session wrangler-proto --new-session-with-layout "$layout"
