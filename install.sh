#!/bin/sh
# Install the client, and print the layout block for the plugin that goes with
# it.
#
# The client is one binary holding the hook the agents invoke, the daemon those
# hooks feed, and this installer. Nothing starts the daemon here: the first hook
# to find none running starts it, by running this same binary.
#
# Both halves come from one release and are named for its tag, so the block this
# prints pins the plugin to the version of the client it just installed. Nothing
# downloads the plugin here: zellij fetches it itself from the url in the layout.
#
# Usage: install.sh [version]   (default: the latest release)
set -eu

repo=JimiSmith/zellij-agent-wrangler
bin=${AGENT_WRANGLER_BIN:-$HOME/.local/bin}

case "$(uname -s)/$(uname -m)" in
    Linux/x86_64) target=x86_64-unknown-linux-gnu ;;
    Linux/aarch64 | Linux/arm64) target=aarch64-unknown-linux-gnu ;;
    Darwin/arm64) target=aarch64-apple-darwin ;;
    Darwin/x86_64) target=x86_64-apple-darwin ;;
    *)
        echo "no released client for $(uname -s)/$(uname -m)." >&2
        echo "build one: cargo build --release -p agent-wrangler" >&2
        exit 1
        ;;
esac

# `gh` is used where it is there, because it is also what reaches a private
# repository; the api is the fallback for a machine without it.
have_gh=$(command -v gh >/dev/null 2>&1 && echo yes || echo no)

version=${1:-}
if [ -z "$version" ]; then
    if [ "$have_gh" = yes ]; then
        version=$(gh release view --repo "$repo" --json tagName -q .tagName)
    else
        version=$(
            curl -fsSL "https://api.github.com/repos/$repo/releases/latest" |
                sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1
        )
    fi
fi
[ -n "$version" ] || { echo "could not work out the latest version." >&2; exit 1; }

client="agent-wrangler-$version-$target"
wasm="zellij-agent-wrangler-$version.wasm"

mkdir -p "$bin"
if [ "$have_gh" = yes ]; then
    gh release download "$version" --repo "$repo" --pattern "$client" \
        --output "$bin/agent-wrangler" --clobber
else
    curl -fsSL --output "$bin/agent-wrangler" \
        "https://github.com/$repo/releases/download/$version/$client"
fi
chmod +x "$bin/agent-wrangler"

"$bin/agent-wrangler" install-hooks

# The sidebar reaches the daemon by running the client, so it has to be able to
# find it. Left to itself it looks on `$PATH`; where that would find nothing, or
# would find some other copy first, the layout has to name this one outright.
# Getting this wrong costs every agent row and says nothing about why, so the
# block printed below is the one that is right for this machine rather than the
# one that is usually right.
url="https://github.com/$repo/releases/download/$version/$wasm"
found=$(command -v agent-wrangler 2>/dev/null || true)

if [ "$found" = "$bin/agent-wrangler" ]; then
    note="The sidebar finds the client on your PATH."
    block="    pane size=32 borderless=true {
        plugin location=\"$url\"
    }"
else
    if [ -n "$found" ]; then
        note="Your PATH finds a different agent-wrangler first ($found), so the
block above names this one outright."
    else
        note="$bin is not on your PATH, so the block above names the client
outright. Put $bin on your PATH instead and you can drop that line."
    fi
    block="    pane size=32 borderless=true {
        plugin location=\"$url\" {
            install_hooks \"$bin/agent-wrangler\"
        }
    }"
fi

cat <<BLOCK

Installed $("$bin/agent-wrangler" --version) to $bin.

Give every tab a sidebar by putting this in your zellij layout, inside both
default_tab_template and new_tab_template:

$block

$note

Zellij downloads that once and holds it. Updating means running this script
again and changing the version in the url to match: the url is what zellij
tells one build of the plugin from another.
BLOCK
