# Progress

Where the port stands, how it is tested, and what zellij turned out to do. The
feature list and its checkpoints are in `FEATURES.md`; running it is in
`README.md`. This file is for what neither of those says: the facts the design
rests on, and what they cost to find.

## Where it stands

Checkpoints 1 to 3 are done bar one item. The sidebar draws the session's real
tabs and panes, `Enter` and a click go to what a row points at, a sidebar leaves
a tab it is alone in, `q` turns them all off for the session, and the sidebars of
a session share one selection. The layout places every sidebar, including in
tabs opened later.

Left in checkpoint 3: turning the sidebar back on after `q`, which needs a
zellij key binding rather than plugin code. Width sync was dropped by decision.

Next is checkpoint 4, agent rows, which is the first that needs the native hook
binary to exist.

## How it is tested

`cargo test` covers everything that does not call zellij: the row model, the
paint, and the reading of tabs and panes into it. That is why the plugin's own
logic lives in `src/lib.rs` and the plugin in a wasm-only bin. Host functions do
not link on the host target, so anything calling them cannot be unit tested.

Everything else is checked by driving a real session. `zellij action
dump-screen` returns nothing for plugin panes, so the only way to see what a
sidebar drew is to run zellij on a pty, capture what the terminal received, and
replay it into a character grid. Alongside that, `zellij --session X action
dump-layout` answers structural questions (which tabs hold what) without any
screen scraping, and `list-clients` says which pane holds the focus. The scripts
doing this were session-scratch and are not in the repo.

Two traps in that harness, both of which produced false results before they were
noticed: a capture must be cumulative from the start of the session rather than
reset between steps, since a frame is only the cells that changed; and zellij
writes each attribute as its own SGR sequence, so a replay has to accumulate
attributes rather than replace them.

## What zellij does

Every line here was established by measurement or from the source, and several
of them contradict what looks reasonable from the outside.

**Plugins are frozen when their tab is not visible.** A plugin whose tab has
fallen behind stops receiving `PaneUpdate` and `TabUpdate`. Its manifest keeps
whatever it last held, which can be arbitrarily stale. Any design where a
background instance is responsible for noticing something is unsound.

**A plugin's identity is its url and its configuration together.** Zellij adds
`caller_cwd` to the configuration of an instance it launches, so a message
addressed to a plugin url never reaches instances a layout started: it launches
another one instead ([#5234](https://github.com/zellij-org/zellij/issues/5234)).

**An unaddressed `pipe_message_to_plugin` reaches every plugin instance**,
sender included, with `source` naming the sender. That is how the sidebars share
a selection. It needs `MessageAndLaunchOtherPlugins`; without it the send is
dropped with no error at all
([#3366](https://github.com/zellij-org/zellij/issues/3366)).

**The permission prompt is not painted when it is raised.** A plugin is not
rendered at all while its request stands, and zellij paints its own prompt only
once something else forces a redraw, so a first run looks like nothing happened
([#4749](https://github.com/zellij-org/zellij/issues/4749)). Grants are cached
in `~/.cache/zellij/permissions.kdl` against the plugin's path, so a rebuild
keeps them, and every tab that existed before the answer asks separately.

**`children` must be a direct child of a tab template.** Nested inside another
pane it is dropped when zellij builds a tab at runtime. A `new_tab_template`
declared beside the default one, with a plain pane where `children` would be, is
what gives a runtime tab the same shape: it needs a pane, not a placeholder.

**A plugin cannot set its pane's size.** Tiled panes resize by `Increase` and
`Decrease` steps only.

**`get_focused_pane_info()` and the session snapshot can each be a step behind**,
and which one is stale alternates. Neither is authoritative on its own around a
tab switch. Both are otherwise better than the per-pane `is_focused` flag, which
cannot say where *this* client is when several are attached.

**Rendering is all-or-nothing.** Every render reprints the whole pane, so an
animated indicator at 16fps cost 19% of a core. Turn state is two static glyphs
for that reason, and the plugin subscribes to no clock.

**Sessions start in locked mode**, which passes keys through to the focused pane
rather than swallowing them, so the sidebar works while locked. `zellij --layout
X --session Y` attaches instead of creating; `--new-session-with-layout` creates.
Closing a pane leaves the focus on the sidebar, not on the surviving terminal.

## Decisions

**Per-tab instances, no background owner.** Each sidebar resolves the whole
session for itself; only the selection is shared, by broadcast. A headless owner
was the alternative and is worse: background plugins cannot be granted
permissions ([#4982](https://github.com/zellij-org/zellij/issues/4982)).

**The layout places sidebars.** Two rounds of plugin machinery for opening
sidebars were written and deleted. Nothing about placement belongs in the
plugin.

**Selection travels as an absolute key**, never a movement, so instances cannot
drift apart.

## A note on method

Two of the three detours in this port came from concluding "zellij cannot do
this" from an experiment. An experiment shows what fails; it cannot rule out an
API that has not been read about. Check the documentation and the issue tracker
before designing around a limitation, and treat mounting workaround complexity
as evidence the premise is wrong.
