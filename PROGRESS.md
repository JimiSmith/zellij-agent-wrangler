# Progress

Where the port stands, how it is tested, and what zellij turned out to do. The
feature list and its checkpoints are in `FEATURES.md`; running it is in
`README.md`. This file is for what neither of those says: the facts the design
rests on, and what they cost to find.

## Where it stands

Every checkpoint is done bar one item. The sidebar draws the session's real tabs
and panes, `Enter` and a click go to what a row points at, a sidebar leaves a
tab it is alone in, `q` turns them all off for the session, and the sidebars of
a session share one selection. The layout places every sidebar, including in
tabs opened later. A pane running an agent is drawn as that agent, labelled with
what the session calls itself, and goes back to being a pane when the agent
ends, and says whose turn it is: `○` mid-turn, `●` when it wants you, answered
by going to its pane. The calls for the user are listed in the notification area
at the foot, newest first, opening one goes to where that agent is now, and a
desktop notification carries the same event out of the terminal. The layout
configures all of it, sections mode included. Both halves are released together
and installed by one script.

Agent state lives in a daemon rather than in the sidebar: one per user, started
by the first hook that finds none running, watching every transcript it holds
and reaping sessions whose process has gone. It knows nothing about panes. The
native half builds for Linux, macOS and Windows.

Left in checkpoint 3: turning the sidebar back on after `q`, which needs a
zellij key binding rather than plugin code. Width sync was dropped by decision,
and so was the bell.

What is left is not a checkpoint but a list: the record-to-pane rules the
original has for a session whose pane it cannot take at face value (pid
ancestry, title matching), and the selection falling back to a nearby row rather
than to the first one when the row it was on goes.

## How it is tested

`cargo test` covers everything that does not call zellij: the row model, the
paint, the reading of tabs and panes into it, the agent registry and its wire
format, the options, the reading of a hook body, and the reading of a title off
disk. That is why the plugin's own logic lives in `src/lib.rs` and the plugin in
a wasm-only bin. Host functions do not link on the host target, so anything
calling them cannot be unit tested.

Three crates separate the halves. `agent-wrangler-core` is the facts, and builds
for wasm as well as the host; `agent-wrangler` is the daemon, the hook and the
installer, and never sees `zellij-tile`; `zellij-agent-wrangler` is the plugin.
Bare `cargo build` on the host is the one command that does not work: it tries
to link the plugin bin, whose host functions exist only inside zellij.

`cargo check -p agent-wrangler --target x86_64-pc-windows-msvc` is how the
Windows port is kept honest from a Linux machine. It needs no linker, so it
catches everything except what only fails at run time.

Everything else is checked by driving a real session. `zellij action
dump-screen` returns nothing for plugin panes, so the only way to see what a
sidebar drew is to run zellij on a pty, capture what the terminal received, and
replay it into a character grid. That harness is now in `tests/`: `screen.py` is
the grid, `drive.py` runs a script of steps against a pty, and
`tests/scripts/agent_row.steps` drives the whole path end to end, from a hook
typed into a real pane to the row it draws and the call it answers.

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
background instance is responsible for noticing something is unsound - including
noticing that it is no longer the one in front of the user, which is the shape
this trap takes when the stale thing is the focus rather than the panes.

**Pipe messages are not subject to that.** `pipe_messages` in
`plugins/wasm_bridge.rs` walks every running plugin with no visibility filter,
so a sidebar in a tab nobody is looking at still hears one. This is what lets a
state message reach every sidebar of a session at once, whichever tab is in
front.

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
included, because the WASI context is built with `inherit_env`. It does not
inherit the filesystem: the four mounts are all there is, so anything in the
user's home directory - an agent's own settings, for one - can only reach the
sidebar by being sent to it.

**A hook is not enough to keep a row current.** An agent fires hooks throughout
a turn and not at all between them, and a slash command that changes what a
session is called or what colour it is (`/color`) submits no prompt, runs no
tool and ends no turn, so no hook fires at all. Measured on a session where the
hooks ran at 08:10:00 and the colour was set at 08:10:04, and the row stayed
stale until the next message.

This is the gap the daemon exists to close, and it cannot be closed from inside
a plugin: it needs something reading files on a clock, and a plugin can reach
neither the transcript nor a clock it can trust.

**A Claude transcript records different things at different times.** The color a
session is given and the name a teammate goes by are written once, near the
start; the title is written and rewritten throughout. A fixed window over the
end of the file therefore stops covering the first two as a session grows -
measured on a 3MB transcript, 47 color records in the file and 4 in the last
64KB.

Reading the head as well is the obvious answer and the wrong one. A scan reports
what it can see now, and a record that says nothing about a color is not a
record saying the color is gone: the sidebar keeps the last non-empty value it
was told. So the color needs finding once, while the transcript is still short,
and never again - which the first hook after it is set does. The original solved
it the same way, by remembering across polls rather than by reading more of the
file.

**`PaneInfo` carries no color.** A pane's border color is not among the fields
zellij reports, so a pane's icon has nothing of its own to be drawn in.

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

**A plugin's configuration is the child nodes of its `plugin` block**, plus any
properties on the block itself, with `location`, `path` and
`_allow_exec_host_cmd` reserved. Every value arrives as a string whatever it was
written as, so `sections true` and `sections "true"` are the same thing
(`parse_plugin_user_configuration` in `kdl_layout_parser.rs`). Bare arguments
are dropped rather than read as flags.

**A remote plugin is cached under the last segment of its url, and a name
already there is never fetched again.** `Downloader::download` returns early on
`file_path.exists()`, and the name is `parse_name`'s last path segment
(`downloader.rs`). Two releases of a plugin under one file name are one file: a
published wasm has to carry its version in its name for an update to arrive at
all.

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

What it answers with is a `PaneId`, so it says whether a plugin pane holds the
focus and which one, and a sidebar can ask whether the focus is its own. Keeping
that id rather than reducing it to "some pane, or none" is what lets the
selection bar be drawn by the one sidebar a keystroke would reach: the number
alone could not, since zellij counts plugin panes and terminal panes in separate
sequences and the same number is two different panes.

**The tab it answers with is the tab's id, and everything else zellij says about
tabs is a position.** The two are the same number until a tab that is not the
last one is closed: ids are handed out as `last + 1` and never reused, while
`close_tab_by_id` shifts the positions of everything after the tab it removed.
Three tabs whose middle one has gone are ids 0, 2 and 3 sitting at positions 0, 1
and 2. `PaneManifest` is keyed by position, `TabInfo.position` is a position, and
`switch_tab_to` counts positions from one; only the focus is an id, and
`TabInfo.tab_id` is what turns it back into a position. Reading the id as a
position drew the wrong tab as the focused one, or no tab at all, and quietly
moved which sidebar believed it was the one the user was standing in. That is why
`TabId` and `TabPosition` are separate types: the mistake is a number matching a
number, and nothing but the type system was ever going to catch it.

**Rendering is all-or-nothing.** Every render reprints the whole pane, so an
animated indicator at 16fps cost 19% of a core. Turn state is two static glyphs
for that reason, and the plugin subscribes to no clock.

**The first click on an unfocused pane never reaches it.** Zellij spends it on
focusing the pane, so a plugin gets no `Mouse` event at all and the click after
it is the one that acts. `mouse_click_through` turns that off and is `false` by
default (`input/options.rs`). It is a session-wide setting rather than anything
a layout can say about one pane, so a sidebar that wants one-click rows has to
ask the user for it.

**Sessions start in locked mode**, which passes keys through to the focused pane
rather than swallowing them, so the sidebar works while locked. `zellij --layout
X --session Y` attaches instead of creating; `--new-session-with-layout` creates.
Closing a pane leaves the focus on the sidebar, not on the surviving terminal,
and switching to a tab restores whatever it was left on - which, once that has
happened, is that tab's sidebar. A row that points at a tab therefore points at
its first pane instead: a pane brings its tab with it, so it is the same one
move and it lands somewhere.

**A plugin cannot make a host call while handling a message from the command
line.** `zellij pipe` waits for the plugin to finish, and a synchronous request
back into the server during that handling waits on the server in turn: the CLI
blocks until zellij gives up ("Action CliPipe did not complete within 1s"), the
answer comes back as an error, and the sidebar resolves against nothing. So
where the user is is asked for on the events that move the focus, and remembered
for anything a pipe triggers.

What is safe there is a command that wants no answer back. `run_command` posts
the command and reads nothing, so raising a desktop notification while handling
a hook's pipe does not wait on anything: it is the *reply* that deadlocks, not
the crossing. Measured, not reasoned about.

**Asking for a permission that was not asked for before asks the user again.**
The cache in `~/.cache/zellij/permissions.kdl` holds the set that was granted
against the plugin's path, and it holds the *last* such set rather than their
union: a run that asks for less overwrites what a run asking for more was
granted. So turning an option that runs commands on prompts, turning it off
quietly drops the grant, and turning it on again prompts a second time. That is
the price of those options, and it is why the sidebar asks for `RunCommands`
only when one of them is in use.

**A plugin cannot ring the terminal bell.** A plugin pane owns a `Grid` and its
output is parsed by it, so a printed `\x07` does set `ring_bell` — but `has_bell`
is only implemented for terminal panes and defaults to `false` in the `Pane`
trait, so nothing ever reads it. There is no bell command or action either.

## Decisions

**Where the user is travels; nobody guesses at it.** A sidebar is sent events
only while its own tab is on screen, so at most one of them can read the focus
at any moment, and the rest hold whatever it was when their tab was last looked
at - which is, by definition, the moment before the user left. So the one that
can read it tells the others, exactly as the selection travels.

That was originally left to each sidebar to work out, on the reasoning that they
all read the same focus and would reach the same answer. They do not: a sidebar
that is hearing nothing reads nothing. Two things fell out of it. A call
answered by going to the agent's pane stayed unanswered in the sidebar that sent
you there - moving the focus off its own tab is exactly what stops it hearing
what happened next, so the one sidebar that could not see the answer was always
the one that caused it. And every sidebar left behind still believed it was the
one in the tab you are in, which is the rule that decides who acts: three tabs
raised three desktop notifications for one call.

**One sidebar acts for all of them: the one in the tab you are in.** Every
sidebar hears every pipe, so anything that must happen once needs a rule they
all read the same way and only one answers to. With the focus shared, being
where the user is is that rule: exactly one tab is active, so exactly one
sidebar is in it. It is what raises one desktop notification per call rather
than one per tab, and what installs the hooks once. Installing also broadcasts
that it has happened, because that one is not idempotent in the way that
matters: two processes writing one settings file is a race whatever each of them
writes.

**What a session is called by travels; what its row says does not.** The client
reports the directory, the title and the teammate name; composing a label out of
them is the sidebar's, so the label option changes every row at once and no
agent has to report itself again. It is also why every event carries the whole
record rather than naming a session already filed: a title taken on mid-session
then arrives on the next event of any kind, and a session whose start nobody
heard is filed by whichever event is heard first.

**Every record says which format it is written in.** The halves are installed
and updated separately, so one can be older than the other. Without the format
an out-of-date client's records would simply fail to parse and the symptom would
be agents that never appear; with it the sidebar can say what is wrong at the
top of the pane.

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

**The agent registry is a daemon's, and every sidebar is a copy of it.** The gap
that decided this was liveness rather than persistence: an agent titles itself,
or is given a colour by `/color`, with no hook firing at all, and nothing that
only listens to hooks can ever see it. A daemon watching transcripts can. The
same process solves the two things that came with it for free: a session
survives every sidebar closing, and an agent killed without an `end` event is
reaped rather than left as a row nothing will remove.

Persistence made reaping compulsory rather than nice. A record brought back from
disk names a process that may have died while nothing was running, so only
records naming a live process are restored; a live agent says so again on its
very next event, where a dead one would otherwise be drawn for good.

**A record reaches the tree only through the pane it names.** An agent whose
pane is gone is still held but drawn nowhere, so nothing has to prune the
registry against the manifest — which matters, since a sidebar in a background
tab holds a manifest that may be arbitrarily stale.

**What an agent is, and what draws it, are separate crates.** `agent-wrangler-core`
holds the record, the turn, what a session is called by, the transcript reads
that find those, and the registry that files them. It names no pane, tab, window
or row. What stayed behind in the plugin is everything that turns a fact into a
row: the label spelling, the terminal color a color *name* is drawn in, and
placing a record under the pane it reported itself from. That line is the same
one the record format already drew, which is why the split moved no logic and
changed no test: 137 tests before, 137 after. The hook client left with core and
is now `agent-wrangler`, one binary that knows nothing about zellij except the
pipe it currently delivers on.

**A `pane: u32` still rides on the record.** It is the one multiplexer-shaped
field in a crate that is supposed to have none. It stays for now because the
wire format has not changed yet; it is replaced by the verbatim environment
variables the hook captured when the daemon lands, and the plugin reads its own
multiplexer's keys out of them.

**An empty message and an empty state are the same bytes, so one has to say so.**
The daemon sends the whole state on every change, and a client takes it as the
truth, so a message that arrives empty is an instruction to forget every agent
there is. That is fine when the daemon means it and a disaster when it does not,
and the two are indistinguishable. Every state message now leads with a line
naming itself and the format its records are in; anything else is ignored. This
was found the hard way, watching a row appear on screen and vanish a second
later.

**A hook finds its agent by name, then by not being a shell.** The pid is what
makes reaping possible, and a hook is a descendant of the agent with a shell or
two in between. Counting steps breaks the moment a shell is added or removed, so
the nearest ancestor named for the agent wins; and because an agent installed
through npm reports as `node`, or as `node-MainThread`, the fallback is the
nearest ancestor that is not a shell. Both rules were written after watching the
name-only version find nothing on this machine.

**`zellij pipe` reaches a session from outside it, and a payload given as an
argument survives intact.** This is the assumption the whole daemon rests on and
it is not the one the hook client used to make. Two things worth knowing came
out of testing it: a payload left empty makes the command read standard input
instead, so a caller that means to send nothing must still redirect; and the
standard-input form sends a further, empty message at end of stream.

**Zellij ignores a relative `XDG_CACHE_HOME`.** It finds its directories through
the XDG rules, which take only absolute paths, so a test that sets a relative one
silently reads the developer's real configuration and permissions instead. A
sidebar that comes up blank because the permission it now asks for was never
granted looks exactly like a sidebar that is broken.

**Nothing slow may happen while the state is held.** The daemon reads an agent's
files, runs a client and writes its state out, and any of those can take
arbitrarily long: a hung network mount, a dead sshfs, a named pipe with no
writer. Doing one of them under the lock stops every other event on the machine
being recorded, and because the daemon keeps answering its socket while stuck,
the recovery this design relies on - a fresh daemon taking the name back -
cannot fire either. So reading is separated from filing on both paths: a hook is
read, then applied; a sweep is planned, looked at, then taken in. The lock covers
only the two ends.

**Two writers naming one temporary file tear the state.** The save wrote to
`agents.json.new` and renamed it over the real file, which is atomic for one
writer and not for two: the second truncates the first's file, the first renames
it into place, and the second then writes through its own descriptor into the
file that is now live. The result parses as nothing, and reading nothing back
means every session has ended. The name now carries the process and a counter.
Worth knowing for the test: writers producing identical bytes tear into
something that still reads back correctly, and looking only at the end finds
whatever the last writer left, which is always whole. It takes distinct writers
and a reader running alongside them.

**A client registers once and cannot tell it is talking to a new daemon.** So
the daemon keeps its clients on disk beside the sessions. Without that, any
restart - a version mismatch, a crash, `dev.sh` - leaves every sidebar drawing
whatever it last received, for good, with nothing said about why. For the same
reason a client is given up on after several refusals rather than one: a single
delivery that failed for a passing reason would otherwise retire it permanently.

## A note on method

Two of the three detours in this port came from concluding "zellij cannot do
this" from an experiment. An experiment shows what fails; it cannot rule out an
API that has not been read about. Check the documentation and the issue tracker
before designing around a limitation, and treat mounting workaround complexity
as evidence the premise is wrong.
