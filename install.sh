#!/bin/sh
# Install the client. Print the layout block for the plugin that goes with it.
#
# The client is one binary. It holds the hook that the agents invoke, the daemon
# that those hooks feed, and this installer. This script does not start the
# daemon. The first hook that finds no daemon starts it, and it runs this same
# binary.
#
# Both halves come from one release, and the tag of that release names them. The
# block that this script prints therefore pins the plugin to the version of the
# client that this script installed. This script does not download the plugin.
# Zellij fetches the plugin itself from the url in the layout.
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
        echo "build the client: cargo build --release -p agent-wrangler" >&2
        exit 1
        ;;
esac

# This script uses `gh` where `gh` is present, because `gh` also reaches a
# private repository. On a machine without `gh`, the api is the fallback.
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
[ -n "$version" ] || { echo "this script did not find a version." >&2; exit 1; }

client="agent-wrangler-$version-$target"
wasm="zellij-agent-wrangler-$version.wasm"

mkdir -p "$bin"

# The download goes beside the real file. A rename then puts it in place. This
# script never writes to the real file.
#
# A write in place keeps the file that the system already knows. macOS validates
# a signed binary against the record for that file. New content under that record
# kills every later run, and macOS gives no reason. A rename puts a new file
# there instead. A program that already runs continues from its own file, and the
# next run gets the whole new file.
#
# A download that stops halfway also leaves nothing to run. The temporary file
# holds the half-written content, and the rename happens only after the download
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

# The block always names the client in full. The sidebar runs the client to
# reach the daemon. Without a path, the sidebar looks on `$PATH`. The `$PATH`
# that matters belongs to the zellij server, which took it from the program that
# started zellij. That program is not always the shell of this script. The full
# path is the only value that is true for every start of zellij. A wrong value
# costs every agent row, and it gives no reason.
url="https://github.com/$repo/releases/download/$version/$wasm"
found=$(command -v agent-wrangler 2>/dev/null || true)

block="    pane size=32 borderless=true {
        plugin location=\"$url\" {
            install_hooks \"$bin/agent-wrangler\"
        }
    }"

if [ -n "$found" ] && [ "$found" != "$bin/agent-wrangler" ]; then
    note="Your PATH finds a different agent-wrangler first ($found).
The block names the client that this script installed. The sidebar therefore
runs that client, whatever the PATH says."
else
    note="The block names the client that this script installed. The full path
is necessary even when $bin is on your PATH.
Zellij must find the client, and zellij gets its environment from the program
that started it, and not from this shell."
fi

cat <<BLOCK

This script installed $("$bin/agent-wrangler" --version) to $bin.

To give every tab a sidebar, add this block to your zellij layout. Put the
block inside both default_tab_template and new_tab_template:

$block

$note

Zellij downloads the plugin once and holds it. To update, run this script
again. Then change the version in the url to match. Zellij tells one build of
the plugin from another by the url.
BLOCK
