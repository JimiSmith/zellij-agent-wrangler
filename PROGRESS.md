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

Work on a tmux client began. The daemon now delivers to a socket as well as to
a zellij pipe, and `tmux-agent-wrangler` is the first program to read one. It
registers a socket named for its server and its session, connects, and writes
out every record. It draws nothing and it reads no tmux topology. The sidebar,
the drawing and the tmux topology are later work. The release attaches no such
binary yet, by decision.

The daemon now reads two more things out of a transcript, in the pass it already
runs. The first is the most recent record that says something. The second is the
tool call that no result answers yet. Both travel as they were read, and the
record format is 7.

The zellij sidebar draws them. The space key opens the row under the cursor.
The block holds what the agent last said, the time it said so, and the tool it
runs now. The message draws as the markdown it was written in. Several rows are open at once, and a row stays open across a state
message. The block is as tall as the message needs, so the pane scrolls, and a
row past the foot is drawn and reachable. Each sidebar keeps its own open rows.
The tmux client reads no key events, so it draws no block.

Delivery no longer decides which clients the daemon keeps. A client of either
kind is kept for as long as it speaks, and given up on after ninety seconds of
silence. A client with nothing to report beats every thirty seconds, on the
transport that already carries its state.

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
to link the plugin bin, whose host functions exist only inside zellij. So the
build under test is named one crate at a time, which is what the release
workflow does.

`tmux-agent-wrangler` tests itself against a stand-in daemon. A test that needs
one binds a socket named for this process and answers on it. No test reaches the
real daemon. Two things resist that approach: the words of `tmux
display-message` and the words of `agent-wrangler register`. A test reads each
command instead of running it. Both are contracts with another program, and a
mistake in either compiles and passes every test that does not read it.

Two more tests cover what no socket in this crate can produce. Dropping either
end of a local socket is a clean close on both systems. So a reader and a writer
that fail on purpose stand in for a socket. The first proves that a failing read
means the daemon went. The second proves that a failing write does not.

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
`tests/scripts/dashboard_view.steps` drives the same path into the other view,
and asserts on the columns of the table and on the word that the TURN column
draws for a call. Every run opens a layout that the harness owns:
`tests/tree.kdl` by default, and `tests/dashboard.kdl` for that one. `dev.kdl`
is where a developer tries the sidebar by hand, so a change there must not move
what a run asserts on.
`tests/scripts/two_sidebars.steps` does the same with two sidebars on one held
pipe, which is the case where two writers share one stream.
`tests/scripts/held_pipe.steps` counts the pipes alive between two publishes,
kills the one it finds, and asserts that the next publish opens another.
`tests/scripts/answered_everywhere.steps` answers a call in one tab and asserts
that the call stops being drawn in the other one, which only the state coming
back can do. `tests/scripts/sidebar_beats.steps` reads the daemon's own monitor
stream and fails if the sidebar never says that it is there. That one asserts on
no cell at all. A sidebar draws the same whether or not it beats, until the
daemon gives up on it ninety seconds later.

The harness names its own user. The daemon's socket is named for the user and
for nothing else, so a developer with the real client installed had every run
reporting to that daemon and asserting on it. The build under test was never
exercised at all, and the first thing to notice was a change that the old daemon
could not carry.

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

**A CLI pipe can be held open, and it carries messages both ways.** `zellij
pipe` given no payload argument reads its stdin and stays alive, and one process
then serves any number of messages. Stdin is framed by the line: one newline
ends one message, and that newline stays in the payload, so a payload must carry
no raw newline of its own. A line of 100,000 bytes arrives whole. The plugin
learns the pipe's id from `PipeSource::Cli`, a fresh UUID per pipe process, and
answers on the same pipe with `cli_pipe_output`. Two instances of one plugin on
one unaddressed pipe both receive every message and both answer on it; answers
of 70,000 bytes never spliced and not one byte was lost. None of this is
documented behaviour. It was measured on zellij 0.45.0 with a throwaway plugin
and five drivers, and it is worth measuring again when zellij moves on.

**`cli_pipe_output` needs `PermissionType::ReadCliPipes`.** Without it the call
is dropped in silence: no error, no log, and nothing on the pane. With it, 40
messages produced 41 answers in order. Receiving on a pipe needs no permission
at all, which is why this was never noticed before something had to answer.

**A plugin can only write on a pipe while it is handling a message from that
pipe.** `cli_pipe_output` called from `update`, which is where a focus change
lands, produces nothing on the pipe. It is not lost: zellij holds it, and the
next message on that pipe hands over everything held, in order and ahead of the
answer to that message. Calling it from `pipe` for a message that came from
another plugin does not open the channel either, so it is the source of the
message and not the callback that decides. Measured on zellij 0.45.0. This is
the one thing the first six probes did not test, because every one of them
answered the message it was handling.

**Every message on a CLI pipe logs `Action CliPipe did not complete within 1s
timeout`.** It is neither a timeout nor an error. `route.rs:1663` drops the
action's completion channel on purpose, so that a held pipe is not sent an
`Exit`, and `route.rs:75` matches a dropped sender and an elapsed timeout in one
arm, so a deliberate, instant drop prints the timeout text. The drop happens
before the message reaches the plugin thread, so nothing is lost, delayed or
corrupted, and no thread waits. Nothing avoids it: the drop is the first
statement in the arm, before any branch, so no flag, no `--plugin` and no
plugin-side call reaches it. Upstream issue #5261 describes it and PR #5264
fixes it, both open, with no release carrying the fix. The one real cost is that
zellij's log rolls at 16 MiB, so a fast beat pushes out the records a person
came to read.

**The pipe process does not exit when its session is killed.** It was still
alive thirty seconds later, with no exit code and an empty stderr. A daemon that
only forgets the client therefore leaves the process behind for as long as the
user stays logged in. A pipe into a session that was never there is the opposite
and exits 1 within milliseconds.

**`block_cli_pipe_input` covers the pipe and not the plugin that called it.** In
the test, one plugin blocked and a second plugin that never blocked received
nothing until the first released. Nothing here calls it.

**A terminal pane's id is `$ZELLIJ_PANE_ID`.** Zellij sets it to the pane's
`terminal_id` when it spawns the process (`os_input_output_unix.rs`), and that is
the same number the plugin reads as `PaneInfo.id` for a non-plugin pane
(`pane_info_for_pane` in `tab/mod.rs`). Every process started in the pane
inherits it, which is how an agent's hook says where it is running.

**A stacked pane leaves the manifest in zellij 0.45.** The release turns a stack
of panes into a *stack list*. `Tab::select_stack_list_member` keeps one member
in `tiled_panes` and parks every other member in `suppressed_panes`.
`pane_infos` then reports a parked pane with `is_suppressed` set, and the mode
is on by default (`stacked_pane_list.unwrap_or(true)`). A parked pane carries
the geometry of the pane on screen in front of it, and no field of `PaneInfo`
says that a pane belongs to a stack. Version 0.44 has no stack lists: every
member of a stack was a pane of its own.

So the sidebar lists a parked pane only while an agent answers for it
(`session::drawn`). `Enter` on that row works, because zellij takes a parked
member back on screen as it takes the focus (`focus_hidden_stack_list_member`).
A parked pane is never where a tab row sends the user, and never where the
sidebar stands down. A scrollback editor parks a pane in the same way, so an
agent keeps its row while the user reads the output of that pane.

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

## What tmux does

Every line here was checked against tmux 3.7b, or read from the psmux source at
`crates/portable-pty-psmux` and `src/`.

**A control mode client is an attached client, and it sizes the windows.** Its
own size is 80 by 24, because its output is a pipe and not a terminal. The
default `window-size` is `latest`, so the windows of the user snap to that size
the moment a sidebar starts. `refresh-client -f ignore-size` stops tmux counting
that client. Measured: a session forced to 200 by 50 stayed there while a
pipe-attached control client was on.

**A control mode client receives every byte that every pane writes.** They come
as `%output` lines. A sidebar wants the changes and nothing else, and an agent
printing a long transcript would push all of it down that pipe.
`refresh-client -f no-output` stops it.

**Psmux accepts `refresh-client -f` and does nothing with it.** `src/main.rs`
reads `-f` and drops it "for compatibility". So an error check finds nothing.
Psmux also answers a fixed `"focused"` for `#{client_flags}` in `src/format.rs`.
The handshake therefore asks the server to name its flags back, which is a
positive check: a server that does not name `no-output` is left, and the sidebar
asks on a timer instead.

**`tmux -CC` refuses to start without a terminal.** It says "tcgetattr failed:
Not a tty". `-C` alone works with pipes on both ends, and it sends the same
notifications.

**Tmux parses its own command lines, and `#` starts a comment there.** An
unquoted `#{...}` is stripped, and the command answers something else with no
error at all. Every format that this project writes on a control client or a
command line is inside single quotes. Tmux also reads an argument that starts
with a dash as a flag, so every marker that this project prints starts with a
letter.

**Each command joined by a semicolon gets its own reply block.** A line of three
commands gives three `%begin`/`%end` pairs and not one. Joined, their bodies are
exactly what the same commands write when they run as a child process, so one
reader parses either transport.

**A pane id starts with a percent sign, exactly as a notification does.** Only
the reply block tells them apart. A reader that classified by the first
character alone would ask for a fresh reading once per pane it was told about.

**`#{pane_title}` falls back to the host name.** It holds what a program set
with an escape sequence, and a program such as `sleep` sets none. A column of
host names says nothing, so the format asks whether the title is still the host
name and names the running program when it is. Zellij falls back to the command
in the same way, so the two clients draw alike.

**A tmux window index is not its order.** A user sets `base-index` to any value,
and a closed window leaves a gap. The row draws the number that the user types,
so `TabReport` carries that number apart from the position. The position stays the
order, because the shared code joins a tab report to a session layout on it.

**A pane made from outside takes the environment of whoever made it.** A
`split-window` run on the host gives the new pane the host's `PATH` and `USER`,
and not the environment of the tmux client that the harness spawned. This is the
same trap as the daemon socket, in a second form, and `tests/README.md` records
both.

## Decisions

**The daemon chooses the records, and the clients get them raw.** A preview off
the last `assistant` record alone draws blank about half the time. Measured over
six transcripts, 94 of 196 assistant records are a tool call and nothing else.
An agent mid-turn is exactly the agent a user asks about. So the daemon
selects two records rather than one. It extracts no field from either. A client
that later wants a tool's input, the number of words or a second block reads it
out of what it already holds. The wire does not move again.

**A tool call is paired with its result by id, not by position.** The reader
holds every call that the window shows and no result answers, and it drops
one when a result names its id. Measured over 1,906 tool calls, 58 started in a
pair, and only the pairing tells those two apart whichever one comes back first.
The reader parses the result records to do this. That costs 0 extra bytes at the
median window and 11,533 at the largest, once a second per agent. The window
itself holds 39,670 bytes at the median. The byte prefilter that skips the rest
of the window stays.

**Every state message repeats both records.** The busiest of 25 recent sessions
changed its records 3.7 times a minute. Six agents with one of them that busy
cost 72 KiB a minute, which is 1.2 KiB a second. That is too little to buy a
cache with. `Slot::fill` replaces a payload that still waits to go, so the
transport drops deliveries on purpose. A message that carried each line once
would therefore leave a slow client with a blank preview for good.

**The plugin gained a parser and no file access.** The feature that read an
agent's files also carried the JSON. It split in two: `json` turns on the parser,
`native` turns on the file reading and takes `json` with it. The plugin takes
`json` alone. The wasm release artifact went from 1,526,838 bytes to 1,528,893,
which is 2,055 bytes for the parser. Nothing in the plugin calls it yet, so the
linker drops nearly all of it, and the real cost lands with the drawing.

**The sidebar parses, and the client crate scrolls.** The daemon sends both
records raw, so a client reads a message, a time and a tool out of them where it
draws. That reading lives in `agent-wrangler-core`, behind `json`, so one reader
answers for every client and the daemon still opens the files. `agent-wrangler-ui`
took that feature and `tui-scrollview` together. The wasm release artifact went
from 1,528,893 bytes to 1,574,971, which is 46,078 bytes for a parser that is
now called and for the clipping.

**The block draws the message as markdown, and the daemon keeps the line
breaks.** An agent answers in markdown, so a message arrives with headings, list
items, fences and inline emphasis in it. `tui-markdown` parses that and returns
`ratatui-core` text, which is what the rows are already drawn from, so no second
drawing path appears. Its `highlight-code` default feature is off: it pulls
syntect and thirty four other crates, and it renders through escape sequences
that the drawing must read back. The wasm release artifact went from 1,575,418
bytes to 1,921,552, which is 346,134 bytes for the parser and the renderer. One
open row costs 6.1 microseconds a frame on this host, measured over a thousand
parses of a message of four lines, and a closed row parses nothing.

`termimad` came first and could not be used. It depends on crossterm, and
crossterm does not compile for `wasm32-wasip1`, which is the plugin's target.

The client reads the message, so the daemon had to stop flattening it. A record
travels as one line, and the reader replaced every control character with a
space. It now keeps the line breaks of the message, because a heading, a list
item and a code fence each need the line that they start. A tool name and a tool
argument draw on one row, and both stay flat.

**A wrap keeps the whitespace, and a table row is never wrapped.** The first
wrap broke on whitespace and collapsed it, the way the wrap for a notification
does. That drew `| core | records |` for a table cell that the parser had padded
to `| core  | records |`, and it lost the indentation of a code block and of a
nested list. The wrap now drops the whitespace at the break alone. A table row
wider than the field is drawn on one line even so, and the row drawing cuts it
at the edge of the pane. A wrapped table row loses the columns that a table is
for. The crate returns text and no structure with it, so a line counts as a
table row when it starts with one of the four glyphs that the crate draws a
table frame with.

**Markdown draws in intensity and in no color.** Color says what a row is and
whose turn it is. So the preview hands `tui-markdown` a style sheet of its own:
a heading and a bold word are bold, a quote and a code span are dim, a link is
underlined, and deleted text is crossed out. The sheet drops the `#` of a
heading and the fence of a code block as well. Both cost a row or a column in a
field that is around a hundred columns wide, and the intensity already says what
they say.

**The block draws the time in UTC, and says so.** A record spells its own time
as `2026-09-01T05:11:01.469Z`. A local time needs an offset from UTC, and a
plugin in wasm has none without a wire change. So the block draws the date and
the minute of the record's own spelling, and keeps the `Z`. The date is drawn
because a message from yesterday must not read as one from this morning. Story 1
dropped the FOR column for the neighbouring reason. An elapsed time needs a clock
read where the row is drawn, and the pane has no repaint tick.

**A tool draws the one input key that its own tool names.** A tool call carries
an `input` object whose keys the tool itself decides, and no key is common to
every tool. Measured over 111 transcripts on this machine, Bash is 5,093 of the
calls, Edit 1,133, Read 567 and Write 247. A long tail follows them, and it
includes MCP tools. So a small table names the key per tool, and a tool that the
table does not name draws its name alone. Reading whatever key a strange tool happens to carry
would be a guess, and an MCP tool promises nothing.

**What is open belongs to one sidebar, and the selection still does not.** The
sidebars of a session broadcast their selection to each other. They do not
broadcast what is open. A block changes the height of the table, and a shared
block would scroll the sidebar of every other tab the moment the user opened a
row in one.

**A click reaches a pane, and never the open marker.** Zellij reports the
column of a click, and the plugin drops it. So a click on a dashboard row goes
to the pane wherever it lands, the marker included. Opening a row with the mouse
needs that column carried through five places, and it is left for later.

**Both scrollbars are switched off.** `tui-scrollview` draws a vertical
scrollbar when the content does not fit, and that scrollbar takes a column from
the right edge of the pane. That column is the turn marker's. Nothing on the
pane now says that rows sit below the foot, as the tree already does.

**The pane follows the selection, minimally, and shows the head of a block.**
The rule that brings the foot of a selection into the pane runs first. The rule
that brings its head in runs after, and wins. So a block too tall to fit shows
its first line rather than its last. A pane that reaches the first row shows the
heading over it too, because nothing above that row can be selected.

**The control client signals, and a query answers.** Nothing decodes the layout
string in `%layout-change`, which is a checksum and a nested geometry grammar.
Every notification means one thing to this sidebar: ask again. The answer comes
from `list-windows` and `list-panes -s`, which are plain tab separated text that
a test can hand to the reader with no tmux running. The transport differs
between a control client and a child process. The reading does not.

**A capability check and not a `cfg`.** The sidebar asks the server what it can
do, and keeps the control client only when the server names `no-output` back. A
server that cannot do it gets the timer. So psmux works today with no
`cfg(windows)` anywhere, and it gets the faster feed with no change to this code
on the day it grows the flag.

**The timer runs whether or not a control client does.** A control client that
dies leaves no gap in the feed. A tick that arrives while a control client holds
an unanswered question costs one line on a pipe.

**The tmux client owns its own registration.** `reconnect_loop` registers on
every round, because a daemon that restarted has dropped the client record and
released the socket name. `Application::register` fires once, so it cannot be
that path. The registration therefore stays in `client.rs`, and the desktop
notifier arguments go with it. That is one path and not two.

**A registration is a pair, and it is not the session name.** `Application` used
to build `register <multiplexer> <session>`, and the client program accepts
`zellij` and `socket` and nothing else. A tmux sidebar is reached at a socket
name that it derived, and the session keeps the name the user gave it. So the
pair travels as its own input, both halves opaque to the shared crate.

**The sidebar draws on the alternate screen.** A whole-pane drawing has nothing
to scroll back through. On the normal screen a host keeps a history for it
anyway, which is two thousand lines per sidebar in tmux by default, and a user
who scrolls that pane finds blank lines. One frame too tall for the pane would
start filling that history, one screen at a time, and nothing would report it.
Leaving the alternate screen also gives the pane back as the sidebar found it,
which lets a test see that the client restores the terminal when it exits.

**A whole-frame print cost the sidebar its redraw on a resize.** A print puts a
whole pane of bytes on the wire, so the client drew only when the application
changed. A resize changes nothing in the application, and tmux reports a resize
to nobody, so the pane held the frame of the old width until something else
moved. A ratatui `Terminal` reads the size on every draw and writes only the
cells that differ. So the client draws after every event, and the gate that held
the bug is gone.

**Raw mode arrived with the drawing, not with the keys.** Without it the pane
echoes a keystroke over the frame. With it, Ctrl-C no longer raises an
interrupt, so the sidebar reads the input and answers `q` and Ctrl-C itself. A
sidebar that read no key at all would be a pane the user could not leave.

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

**A rename reaches a type name and stops there.** The shared crates and
`proto.rs` were renamed so that a name says what it is. Not one variant and not
one field moved. No type in `proto.rs` carries a `#[serde(rename)]`, so serde
derives every `kind` value from a variant name and every JSON key from a field
name, and a rename of either would move the bytes on a wire that a daemon and a
sidebar of different builds share. A type name serializes nowhere, so every type
name was free. `MonitorRecord::event` was free for the same reason:
`#[serde(flatten)]` lifts the tag and the fields of the `MonitorEvent` to the top
level, so that field name never reaches the wire. Adding a `#[serde(rename)]`
would have bought a few better variant names for a permanent second spelling
that every future reader has to keep in step. That is the cost the rename set out
to remove.

**Some names were seen and left.** The renames covered every name that the rule
plainly forbids. These break it too, and each one needs a decision that a rename
cannot make on its own. In `agent-wrangler-core`: `Record::None` shadows
`Option::None`; `Agent::agent` holds the agent product name on a type called
`Agent` in a module called `agent`; `Registry::start` and `Registry::report`
differ only in which fields the arriving record wins. In `agent-wrangler-ui`:
`render::wrap`, `render::notification_body_field`, `Frame::lines` returns `Row`
values, and `build_tree` returns a flat list. In `agent-wrangler-sidebar`:
`Decision` wraps one `Vec<Effect>` and decides nothing; `Application::render`
takes `&mut self`, caches and renders nothing; `leave_if_alone`, `where_it_is`,
`Application::adopt`, and `UserAction::Click` carries an unlabelled line index.
The daemon's own modules were left whole, `daemon/sink/` included: the wire calls
a delivery target a `sink`, so the field and the module agree with the bytes even
though the type is now `DeliveryTarget`.

**The daemon holds one pipe per session rather than running one per delivery.**
A wasm plugin cannot hold a connection, so the daemon must reach out to it, and
for a long time that meant a process per publish and a process for every word a
sidebar had to say back. A held pipe removes both. The state goes out as one
line on a stdin that stays open, and an answer comes back on the same process's
stdout, so a sidebar answering a call costs nothing at all. `Effect::Run` keeps
only the two things no such pipe can carry: the registration that opens it, and
the hooks, which are files on disk.

**The daemon writes an empty line down each held pipe, at two rates.** It is
what gives a sidebar its turn to speak. Without it the answer to a call sits in
zellij's buffer for ever: the daemon writes only when something changed, and the
answer is the change. The symptom was exact and narrow. The tab you are in
cleared the call from its own records, and every other tab drew it until the
agent reported again.

A second while an agent waits for the user, and thirty seconds the rest of the
time. One rate was tried first and was wrong in a way that only showed up in the
multiplexer's log: every message on a CLI pipe writes a line there, so a beat of
one second wrote nine kilobytes a minute into a log that rolls at 16 MiB, and a
day and a half of history became all that fit. Two rates spend that budget where
it buys something. Idle costs two lines a minute, and the fast beat runs only
between a call arriving and it being answered.

The daemon does not work out which session holds the call. It knows that
somebody is calling, and a machine has few sessions and short calls, so the
saving is not worth teaching the daemon where an agent is shown.

A nudge never stands in front of a payload that has not been written yet,
because that payload is a state a client has not seen.

**Each held pipe has a writer of its own, and a slot that holds one payload.**
The publish path fills the slot and returns, so it never waits on a client. One
slot rather than a queue, because every delivery carries the whole state: a
payload that a newer one replaced is a payload nobody needs, and a client that
reads slowly gets the newest state rather than the oldest. One writer per child
rather than one for all of them, because a session whose pipe buffer filled
would otherwise hold up every other session, which is the failure the held pipe
was meant to remove.

**A held child is asked whether it is alive before it is written to.** The
writer learns that its child died only from the write that failed, which is one
publish after the write. Left at that, a sidebar missed every state until the
publish after that one, and `held_pipe.steps` is what found it: the pipe was
killed, the next agent reported, and its name never reached the screen. One call
that does not wait, on the publish thread, turns two lost publishes into none.

**No delivery outcome says anything about the client it was for.** Two rules
were tried before the one that stands, and each one had a client it could not
see. Counting refused deliveries reads a busy multiplexer as a refusal, and it
never finds a session that lives on with no sidebar in it, which `q` creates and
FEATURES.md lists as a feature. Giving up on a socket with no peer reads the
kernel rather than the process, and a client whose reader thread died holds its
connection open for as long as it lives. What both of those ask is the wrong
question. The right one is whether the client can still send a message, and a
client answers it by speaking. Silence retires a client after ninety seconds,
which is three of its thirty second beats.

**A zellij sidebar cannot choose how often it beats.** A plugin writes on a pipe
only while it handles a message from that pipe, so the daemon's own beat sets
the cadence. That is why the two numbers are the same. It is also why nothing a
sidebar owes the daemon is written the moment it is raised: the pipe id that the
plugin holds between messages can already belong to a pipe that the daemon
replaced, and a line sent to that id reaches nobody. Every line waits in a list
and goes out inside the next message, where the id came in with the message and
is fresh by construction.

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
reason the silence that retires a client is three beats and not one: a single
beat that went missing for a passing reason would otherwise retire it
permanently.

## A note on method

Two of the three detours in this port came from concluding "zellij cannot do
this" from an experiment. An experiment shows what fails; it cannot rule out an
API that has not been read about. Check the documentation and the issue tracker
before designing around a limitation, and treat mounting workaround complexity
as evidence the premise is wrong.
