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

# Downloaded beside the real one and renamed over it, never written to it.
#
# Writing in place keeps the file the system already knows, and macOS validates a
# signed binary against what it recorded for that file: rewriting the contents
# under it leaves every later run killed outright, with nothing said about why.
# Renaming puts a new file there instead, so what was running before goes on
# running from what it started as and the next run gets the new one whole.
#
# It also means a download that dies halfway leaves nothing behind that could be
# run: the temporary is what is half-written, and it is only ever renamed once it
# is complete.
temp="$bin/.agent-wrangler.$$"
trap 'rm -f "$temp"' EXIT INT TERM
if [ "$have_gh" = yes ]; then
    gh release download "$version" --repo "$repo" --pattern "$client" \
        --output "$temp" --clobber
else
    curl -fsSL --output "$temp" \
        "https://github.com/$repo/releases/download/$version/$client"
fi
chmod +x "$temp"
mv -f "$temp" "$bin/agent-wrangler"
trap - EXIT INT TERM

"$bin/agent-wrangler" install-hooks

# The block names the client outright, always. The sidebar reaches the daemon by
# running it, and left to itself it looks on `$PATH` - but the one that matters
# is the zellij server's, inherited from whatever started zellij, which is not
# necessarily the shell this script is running in. Naming the path is the only
# thing that is true whatever started zellij, and getting it wrong costs every
# agent row while saying nothing about why.
url="https://github.com/$repo/releases/download/$version/$wasm"
found=$(command -v agent-wrangler 2>/dev/null || true)

block="    pane size=32 borderless=true {
        plugin location=\"$url\" {
            install_hooks \"$bin/agent-wrangler\"
        }
    }"

if [ -n "$found" ] && [ "$found" != "$bin/agent-wrangler" ]; then
    note="Note that your PATH finds a different agent-wrangler first ($found).
The block names the one this script just installed, so the sidebar runs that
one whatever the path says."
else
    note="That names the client this script installed. Leave the path in even if
$bin is on your PATH: what has to find it is zellij, whose environment comes
from whatever started it rather than from this shell."
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
