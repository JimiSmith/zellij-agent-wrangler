# Progress

Where the port stands, how it is tested, and what zellij turned out to do. The
feature list and its checkpoints are in `FEATURES.md`; running it is in
`README.md`. This file is for what neither of those says: the facts the design
rests on, and what they cost to find.

## Where it stands

Checkpoints 1 to 6 are done bar one item. The sidebar draws the session's real
tabs and panes, `Enter` and a click go to what a row points at, a sidebar leaves
a tab it is alone in, `q` turns them all off for the session, and the sidebars of
a session share one selection. The layout places every sidebar, including in
tabs opened later. A pane running an agent is drawn as that agent, labelled with
the directory it is working in, and goes back to being a pane when the agent
ends, and says whose turn it is: `○` mid-turn, `●` when it wants you, answered
by going to its pane. The calls for the user are listed in the notification area
at the foot, newest first, and opening one goes to where that agent is now.

Left in checkpoint 3: turning the sidebar back on after `q`, which needs a
zellij key binding rather than plugin code. Width sync was dropped by decision.

Next is checkpoint 7: options as plugin configuration, sections mode, label
modes, and distribution. The desktop notification waits for that, since it is
the option that turns it on that is missing rather than the way to raise it -
the hook client can run a command, and is the one process that sees each event
once.

## How it is tested

`cargo test` covers everything that does not call zellij: the row model, the
paint, the reading of tabs and panes into it, the agent registry and its wire
format, and the reading of a hook body. That is why the plugin's own logic lives
in `src/lib.rs` and the plugin in a wasm-only bin. Host functions do not link on
the host target, so anything calling them cannot be unit tested.

The crate's `native` feature is what separates the two halves. It is on by
default, so `cargo test` covers the whole crate; the wasm is built with
`--no-default-features`, which is what keeps the hook client and the JSON it
reads out of the plugin.

Everything else is checked by driving a real session. `zellij action
dump-screen` returns nothing for plugin panes, so the only way to see what a
sidebar drew is to run zellij on a pty, capture what the terminal received, and
replay it into a character grid. Alongside that, `zellij --session X action
dump-layout` answers structural questions (which tabs hold what) without any
screen scraping, and `list-clients` says which pane holds the focus. The scripts
doing this were session-scratch and are not in the repo.

Three traps in that harness, each of which produced a false result before it was
noticed. A shell redirection written to discard a stream can fail to:
`>&2 2>/dev/null` points stdout at the *old* stderr before redirecting, so a
test of "output that is thrown away" was testing the opposite. The other two: a capture must be cumulative from the start of the session rather than
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

**Pipe messages are not subject to that.** `pipe_messages` in
`plugins/wasm_bridge.rs` walks every running plugin with no visibility filter,
so a sidebar in a tab nobody is looking at still hears one. This is what lets
every sidebar hold the agent registry rather than one of them owning it.

**A terminal pane's id is `$ZELLIJ_PANE_ID`.** Zellij sets it to the pane's
`terminal_id` when it spawns the process (`os_input_output_unix.rs`), and that is
the same number the plugin reads as `PaneInfo.id` for a non-plugin pane
(`pane_info_for_pane` in `tab/mod.rs`). Every process started in the pane
inherits it, which is how an agent's hook says where it is running.

**A plugin's `/tmp` is the host's `$TMPDIR/zellij-<uid>`,** readable and
writable both ways; `/host`, `/data` and `/cache` are mounted too. The
documentation says `/data` is "shared with all loaded instances of the plugin",
and it is not: `plugin_own_data_dir` ends in `<plugin_id>-<client_id>` and is
`remove_dir_all`'d when that one instance unloads. `/tmp` is the only one of the
four that two instances, or a plugin and a native process, can meet in.

**A plugin inherits the zellij server's environment**, `ZELLIJ_SESSION_NAME`
included, because the WASI context is built with `inherit_env`.

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

**A pane's title is never reported, but the two things behind it are.**
`PaneUpdate` and `TabUpdate` come from `log_and_report_session_state`, which the
screen calls when the session's *shape* changes; a program renaming its own pane
through OSC 0/2 does not go through it, so a held manifest carries whatever each
title was when a pane was last opened or closed, however long ago.

What does get reported is `CommandChanged` and `CwdChanged`. Zellij's pty thread
already walks the panes that produced output once a second, and pushes those
only when a pane's foreground command or directory actually changed. Both need
only `ReadApplicationState`. The title itself is then one `get_pane_info` round
trip for that one pane. This is why the sidebar keeps no clock: zellij does the
watching once for the whole session rather than once per sidebar, and an idle
session costs 0.1% of a core against 0.9% for polling every pane at 1Hz from
each of four sidebars.

What that misses is a rename which changes neither the command nor the
directory — a program relabelling itself mid-run. That is mostly the case the
sidebar does not want anyway, since an agent's row is drawn from its own record
rather than from the pane's title.

Two things that look like alternatives are not: `SessionUpdate` is driven by the
same shape-change detection rather than by a clock, and `get_session_list`
returns the background job's cached copy after scanning every session on the
machine off disk.

This is also what makes an empty title dangerous rather than merely odd. OSC 0/2
`trim()`s its argument and stores the result, so a program clearing its title on
exit leaves `Some("")` standing, and a manifest sampled then holds a blank title
until something asks again.

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

**A plugin cannot make a host call while handling a message from the command
line.** `zellij pipe` waits for the plugin to finish, and a synchronous request
back into the server during that handling waits on the server in turn: the CLI
blocks until zellij gives up ("Action CliPipe did not complete within 1s"), the
answer comes back as an error, and the sidebar resolves against nothing. So
where the user is is asked for on the events that move the focus, and remembered
for anything a pipe triggers.

**A plugin cannot ring the terminal bell.** A plugin pane owns a `Grid` and its
output is parsed by it, so a printed `\x07` does set `ring_bell` — but `has_bell`
is only implemented for terminal panes and defaults to `false` in the `Pane`
trait, so nothing ever reads it. There is no bell command or action either.

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

**The bell was dropped rather than chased.** Only a terminal pane's own output
can ring one, and a hook has no way to reach that: its stdout and stderr are
captured by whatever invoked it, and it runs with no controlling terminal, so
`/dev/tty` fails with `ENXIO`. What remained was walking the process tree for an
ancestor still attached to a pseudo-terminal, which works and is far too much
apparatus for a beep. The notification area carries the same event.

**The agent registry is held by every sidebar, not persisted.** A hook reaches
all of them at once and each draws the whole session, so they agree without
anyone owning the record; a sidebar opening later asks the others for what they
have and any sidebar that has some answers. Nothing survives every sidebar being
closed at once. Writing the records under `/tmp` would fix that, at the cost of
files no process is responsible for removing: an agent killed without an `end`
would leave one behind for good, where the held version simply forgets. That
trade is worth revisiting only if the gap is actually felt.

**A record reaches the tree only through the pane it names.** An agent whose
pane is gone is still held but drawn nowhere, so nothing has to prune the
registry against the manifest — which matters, since a sidebar in a background
tab holds a manifest that may be arbitrarily stale.

## A note on method

Two of the three detours in this port came from concluding "zellij cannot do
this" from an experiment. An experiment shows what fails; it cannot rule out an
API that has not been read about. Check the documentation and the issue tracker
before designing around a limitation, and treat mounting workaround complexity
as evidence the premise is wrong.
