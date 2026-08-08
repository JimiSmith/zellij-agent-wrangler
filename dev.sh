#!/usr/bin/env bash
# Build the plugin and open a zellij session whose every tab carries it.
#
# A session loads the wasm once and holds it, so an existing `wrangler-proto` is
# killed rather than attached to: attaching would silently run the build before
# last. Anything else in that session goes with it.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
wasm="$root/target/wasm32-wasip1/debug/zellij-agent-wrangler.wasm"

cargo build --manifest-path "$root/Cargo.toml" --target wasm32-wasip1 \
    -p zellij-agent-wrangler
cargo build --manifest-path "$root/Cargo.toml" -p agent-wrangler

client="$root/target/debug/agent-wrangler"
echo "client: $client"

# A daemon already running is one built before this build, and it is the daemon
# every hook and every sidebar will reach. Stopping it leaves the next event to
# start the one that was just built.
ps -e -o pid=,args= | grep -F "$client daemon" | grep -v grep |
    awk '{print $1}' | xargs -r kill 2>/dev/null || true

layout="$(mktemp -d)/dev.kdl"
sed -e "s#PLUGIN_LOCATION#file:$wasm#" \
    -e "s#CLIENT_LOCATION#$client#" "$root/dev.kdl" >"$layout"

zellij delete-session wrangler-proto --force >/dev/null 2>&1 || true

# `--session` alone with `--layout` attaches instead of creating, so the layout
# rides in on the flag that always starts a new session.
exec zellij --session wrangler-proto --new-session-with-layout "$layout"
