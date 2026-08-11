# zellij-agent-wrangler

A zellij sidebar pane listing tabs, their panes as a tree, and the agent
sessions running in them.

`FEATURES.md` is the list of what it does and what is left; `PROGRESS.md` is
what the design rests on.

```
▌ 1: wrangler
  ├─ 1:   nvim
▌ ├─ 2: 󱙺  wrangler
  └─ 3:   cargo watch
  2: notes
  ├─ 1:   nvim
  └─ 2: 󱙺  docs
  3: infra
  ├─ 1:   ssh prod-1
  └─ 2:   k9s
```

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/JimiSmith/zellij-agent-wrangler/main/install.sh | sh
```

That installs the client, wires it into each agent's hooks, and prints the
layout block for the plugin that goes with it. Nothing downloads the plugin:
zellij fetches it from the url in that block and holds it. Nothing starts the
daemon either: the first hook to find none running starts it.

The block it prints names the client's own path. The sidebar reaches the daemon
by running it, and the `PATH` that has to find it is the zellij server's,
inherited from whatever started zellij rather than from the shell you installed
from. A sidebar that cannot run the client draws no agents, and says so at the
top of the pane along with what the run said, since that is why the tree beneath
it is empty.

Put the block in your layout inside both `default_tab_template` and
`new_tab_template`, beside `children`:

```kdl
pane size=32 borderless=true {
    plugin location="https://github.com/JimiSmith/zellij-agent-wrangler/releases/download/v0.1.3/zellij-agent-wrangler-v0.1.3.wasm" {
        install_hooks "/home/you/.local/bin/agent-wrangler"
    }
}
```

Updating is running the script again and changing the version in that url to
match. The two halves are released together and named for the same tag, and each
record the client sends says which format it is written in, so a sidebar being
reported to by a client of another version says so at the top of the pane
instead of quietly drawing no agents.

The url is also how zellij tells one build from another: it caches a downloaded
plugin under the last part of the url and never fetches that name again, which
is why the version is in the file name.

## Building it

```bash
rustup target add wasm32-wasip1
./dev.sh                 # builds both halves, then opens a session with the sidebar in every tab
cargo test               # everything that does not call zellij, on the host target
```

Three crates. `agent-wrangler-core` holds what an agent session is and what it
is called by, and names no pane, tab or row. `agent-wrangler` is one binary
holding the hook client, the daemon and the installer; `dev.sh` prints its path.
`zellij-agent-wrangler` is the plugin, the only crate that depends on zellij's
own: off wasm that brings in curl, openssl and the rest of `zellij-utils`, 9
crates rather than 250, and nothing native ever sees it.

The client is named for what it wrangles rather than for what draws it, because
nothing it does is particular to zellij.

## The daemon

Agent state lives in a daemon, one per user, started by whichever hook first
finds none running. It holds what the sessions are and nothing about where they
are shown; the sidebar is what turns a record into a row.

```bash
agent-wrangler agents      # what the daemon holds, as it would send it
```

A hook says what it saw and exits: which agent, which event, the transcript's
path, and a named few of its own environment variables, captured verbatim. The
daemon does the reading. That keeps the hook off the critical path of the turn
it runs inside, and it is what lets the daemon do the two things a plugin never
could:

- **Watch.** Every transcript it has been told about is looked at once a second,
  and re-read when it has moved. A session that titles itself, or is given a
  colour with `/color`, is drawn without any hook firing at all.
- **Reap.** A session whose process has gone is dropped. An agent killed without
  an `end` event used to leave a row nothing would ever take away.

The daemon and the hook are the same executable, and a hook starts the daemon by
running its own path, so the two can never be different builds. What can differ
is the daemon and the plugin, and a state message names the format it is written
in: a sidebar sent one it does not know says so at the top of the pane.

Records survive the daemon being restarted, but only those naming a process
still running: a live agent says so again on its next event of any kind, where a
dead one would otherwise be drawn for good.

State is kept under `$XDG_STATE_HOME/agent-wrangler` (`%LOCALAPPDATA%` on
Windows). The daemon is reached over a local socket, which is a unix socket on
unix and a named pipe on Windows.

## Agent rows

An agent appears in the tree once its own lifecycle hooks call the client, which
reports the pane it was invoked in. The install script does this for you; to do it by hand, or after moving the
binary:

```bash
agent-wrangler install-hooks            # or: claude, copilot
agent-wrangler install-hooks --uninstall
```

Or have the sidebar do it on load, with `install_hooks` below.

It writes the absolute path of the binary you ran, so it works from wherever
that is; run it again after moving the binary. Claude's hooks are merged into
`~/.claude/settings.json`, keeping every other key, the order they are in, the
file's permissions and a `.agent-wrangler.bak` copy of what was there; only
commands running *this* client are replaced, so hooks belonging to anything else
survive both installing and uninstalling. Copilot's file is one this owns
outright. Running install twice writes the same bytes.

The hooks cover every agent you start anywhere; the client does nothing at all
outside zellij, so sessions elsewhere are unaffected.

Start an agent in a pane and that pane's row becomes the agent's, labelled with
whatever the session has decided to call itself and falling back to the
directory it is working in until it has a name. A teammate is labelled
`@name - title`, so it is never mistaken for a session of its own. A pane
running two agents contributes a row each. `○` at the right edge says the agent is mid-turn and `●` says it wants
you; going to its pane answers the second and leaves the first alone.

An agent's icon is drawn in the color that agent shows for the session (changed
with Claude's `/color`), so the row ties to the session without a list of
full-width colored lines to read past. Color says *which* agent, never which row
is live - where you are is the `▌` gutter. The eight colors Claude names are
drawn from your terminal's own palette, orange and pink in the bright form of
their neighbour, so they follow your theme rather than Claude's.

The agents of a session are known to every sidebar in it, and a sidebar opening
in a new tab asks the others for what they have. Nothing survives every sidebar
being closed at once.

Each run kills the `wrangler-proto` session it opens last time, because a
session holds the wasm it loaded at startup and attaching would run the build
before last.

**The first run comes up blank, and does so again after an upgrade that changes
which permissions are asked for.** Zellij caches the answer against the exact
set that was asked for, so adding one means being asked again. The sidebar needs
zellij's `ReadApplicationState`, `ChangeApplicationState`, `RunCommands` and
`MessageAndLaunchOtherPlugins`, and zellij asks by drawing over the plugin's
pane, but nothing paints the question until something else forces a redraw, and a plugin is not rendered at all while its request is
pending. Focus the sidebar (`Ctrl+g`, then `Ctrl+p` and `←`) and press `y`.
Zellij caches the answer against the wasm's path in
`~/.cache/zellij/permissions.kdl`, so this is once per machine rather than once
per run, and the path does not change when the wasm is rebuilt. Each tab that
existed before the answer asks separately, since every tab's sidebar is its own
plugin instance; tabs opened afterwards find the answer cached. See
[zellij#4749](https://github.com/zellij-org/zellij/issues/4749).

Keys reach the plugin whenever its pane has focus. Zellij starts every session
in locked mode, which turns off zellij's own bindings and passes keystrokes
through to the focused pane, so the sidebar works while locked; `Ctrl+g`
unlocks, which is what you need to move focus onto the sidebar (`Ctrl+p` then an
arrow) or to quit (`Ctrl+q`).

`j`/`k` and the arrow keys move the selection; a click goes to the row it lands
on, as does `Enter`. `q` turns the sidebar off for the whole session.

A tab's row takes you to the first pane that tab lists rather than to the tab
itself. Going to a tab lands wherever that tab was last left, which is as often
as not that tab's own sidebar, and arriving at a sidebar is arriving nowhere.

The selection bar is drawn only by the sidebar your keys would actually reach,
so at most one of them shows one and an unfocused sidebar shows none. Where you
are is the `▌` gutter, and that keeps saying so from every tab. The selection
itself is not lost meanwhile: it comes back where it was the moment the sidebar
takes focus.

A click only reaches the sidebar when the sidebar already has focus. Zellij
spends the first click on focusing the pane, so clicking a row from a terminal
pane moves the focus onto the sidebar and does nothing else, and the click after
it works. Turn that off in your own zellij config:

```kdl
mouse_click_through true
```

The plugin does not set it for you: it decides how every pane in every session
behaves, not just this one.

The sidebars of a session share their selection, so they read as one sidebar
that follows you.

Every tab has one, including tabs opened later: the layout declares a
`new_tab_template` beside its `default_tab_template`, with a plain pane where
`children` would be.

Nerd Font glyphs are used for the pane and agent icons, so the terminal needs a
patched font to draw them.

## Options

Options go in the plugin's own block in the layout, and every one of them is
shown here at its default:

```kdl
plugin location="..." {
    label "name"                 // agent rows: 'name' (session title, falling
                                 // back to the directory) | 'dir'
    sections false               // a block per agent below the tree
    turn_state true              // '○' mid-turn and '●' when it wants you
    notifications true           // the calls for the user, at the foot
    desktop_notification "off"   // 'off' | 'on' (notify-send) | a command line
    install_hooks "off"          // 'off' | 'on' | a path to the hook client
}
```

Put the same block in both templates: a tab opened later is built from
`new_tab_template`, and takes its options from there. `desktop_notification` is
worth keeping the same in both for a further reason: the sidebars of a session
are one client as far as the daemon is concerned, and the last of them to say
what it wants is what that client wants, so templates that disagree settle it by
the order the tabs were opened in.

A value an option does not recognise leaves that option at its default, so a
typo costs you the setting rather than the sidebar.

`sections on` draws the tree and then the same sessions again, gathered under
the agent running them (`CLAUDE`, `COPILOT`, ...) rather than under the tab
they are in. It only groups: a tab, a pane and an agent are drawn exactly the
same wherever they appear.

`install_hooks` names the client as well as asking for the hooks to be
installed, because the sidebar reaches the daemon by running that client and
needs to know where it is. Give it the client's path. `on` means "on your
`$PATH`" and is worth avoiding: the `$PATH` that matters is the zellij server's,
inherited from whatever started zellij rather than from the shell you installed
from, and getting it wrong costs every agent row with nothing said about why.
Both the install script and `dev.sh` write the path in for you.

`desktop_notification` and `install_hooks` both run a command, but so does
asking the daemon for the agents at all, so the sidebar asks for zellij's
`RunCommands` permission whatever these are set to. `install_hooks` is done by
one sidebar while the others stand down - whichever is in the tab you are in,
which every sidebar works out the same way.

`desktop_notification` is not run by the sidebar at all: the sidebar tells the
daemon what to raise a notification with, and the daemon raises it. That is what
makes one call one notification, however many sidebars and sessions are holding
it. Two sessions naming the same command get one notification between them; two
naming different commands get one each.

The notification takes the agent's name and what the session is called as its
last two arguments, which is what `notify-send` wants. It does not name the tab,
where the entry at the foot does: the daemon has never heard of tabs. It is
raised for every call whatever pane is focused, since sitting in a pane says
nothing about whether the terminal is on screen; the `●` and the entry at the
foot still clear when you get to the agent.

`install_hooks "on"` runs `agent-wrangler` from your `PATH`. Name a path
instead if it is not there.

## What it demonstrates

- **The paint.** `render.rs` chooses every glyph that is not the literal name of
  a thing: the gutter, the tree branches, the index prefix, the kind icon, and
  the styling. A row is drawn as styled pieces rather than one styled line,
  which is what lets an agent's color sit on its icon alone.
- **Two regions.** The tree fills the pane; the notification area is pinned to
  the foot, capped at a quarter of it, and admits an entry only if it fits
  whole, so a title never appears over a cut-off message.
- **Input.** Key and mouse events arrive for the plugin's own pane with no
  permission grant, so the prototype prompts for nothing on load.
- **A still pane.** Turn state is two static glyphs, `●` for the agent that
  wants you and `○` for the one still going, and the plugin subscribes to no
  clock. It repaints only when a key or a click changes what it would draw, and
  costs nothing between.

## Layout

- `model.rs` — the row vocabulary: what a row is, where it sits, what its turn
  state is.
- `render.rs` — the line drawn for a row, and the styling of its pieces.
- `session.rs` — the shape of the session, read out of what zellij reports. The
  only place zellij's types meet the sidebar's, and the only module the `plugin`
  feature gates.
- `tree.rs` — the rows a session is drawn as.
- `agents.rs` — the agent sessions, their wire format, and the panes they sit in.
- `options.rs` — what the layout asks for, read into types.
- `command.rs` — a command line written as one string, read into its words.
- `payload.rs` — reading an agent's hook body. Native only.
- `titles.rs` — what a session calls itself, read from the agent's own files.
  Native only.
- `install.rs` — writing the hooks into each agent's config. Native only.
- `main.rs` — the plugin: the two regions, the nav order over them, and the
  event handling.
- `wrangler.rs` — the hook client.
