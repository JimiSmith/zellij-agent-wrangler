#!/usr/bin/env bash
# Build the plugin and open a zellij session whose every tab carries it.
#
# A session loads the wasm once and holds it, so an existing `wrangler-proto` is
# killed rather than attached to: attaching would silently run the build before
# last. Anything else in that session goes with it.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
wasm="$root/target/wasm32-wasip1/debug/zellij-agent-wrangler.wasm"

# The plugin is the crate without its native half: the hook client is a real
# process and has no business inside the wasm.
cargo build --manifest-path "$root/Cargo.toml" --target wasm32-wasip1 \
    --no-default-features --bin zellij-agent-wrangler
cargo build --manifest-path "$root/Cargo.toml" --bin zellij-wrangler

echo "hook client: $root/target/debug/zellij-wrangler"

layout="$(mktemp -d)/dev.kdl"
sed "s#PLUGIN_LOCATION#file:$wasm#" "$root/dev.kdl" >"$layout"

zellij delete-session wrangler-proto --force >/dev/null 2>&1 || true

# `--session` alone with `--layout` attaches instead of creating, so the layout
# rides in on the flag that always starts a new session.
exec zellij --session wrangler-proto --new-session-with-layout "$layout"
