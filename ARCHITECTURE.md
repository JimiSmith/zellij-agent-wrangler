# Architecture

This document describes the intended architecture of the sidebar. It is a
target to move towards rather than a description of every detail of the current
implementation.

The central idea is that events report facts. They update state, but do not
construct the view or directly perform decisions derived from a partially
updated view. Rendering independently derives a complete view model from the
latest state.

## Principles

- Events save facts, mark the application dirty, and produce explicit effects.
- Rendering is independent of event handling and reproducible from state.
- Stable IDs identify tabs, panes, agents, selections, and actions. Positions
  and indexes are presentation details only.
- The rendered view model is the single source for drawing and interaction.
- Uncertain focus is represented as uncertain rather than guessed.
- Side effects are performed from explicit decisions, never while deriving a
  view.
- Pure state transitions and view construction should be testable without a
  Zellij host.

## Overview

The sidebar is split into four conceptual stages:

```text
Zellij events and pipe messages
              |
              v
        state reducer
       /             \
  updated state    effects
       |
       v
 session reconciliation
       |
       v
 sidebar view model
       |
       v
     renderer
```

The reducer records what was observed. Reconciliation turns independently
reported Zellij facts into a coherent session model. View construction turns
that model into the exact lines and actions available in a pane of a particular
size. The renderer translates those lines into terminal output.

## Crates

- `agent-wrangler-core` — agent records, the registry, labels, commands and
  other logic shared by every client and the native daemon. It names no pane,
  tab or row.
- `agent-wrangler-ui` — the row vocabulary, tree and frame composition,
  terminal styling, selection, and ANSI serialization. It draws into a ratatui
  buffer and knows nothing about how one reaches a screen, so a plugin that can
  only print and a program that owns its terminal draw the same sidebar.
- `agent-wrangler-sidebar` — multiplexer-neutral application state, reducer
  inputs, effects, session reconciliation, client state, and configuration.
- `zellij-agent-wrangler` — the plugin. `adapter.rs` converts between Zellij
  reports and the portable sidebar vocabulary; `main.rs` holds the
  subscriptions, effect execution, observation feedback, and printing of the
  rendered frame. It is the only crate depending on Zellij's own, which off
  wasm brings in curl, openssl and the rest of `zellij-utils`: 9 crates rather
  than 250, and nothing native ever sees it.
- `agent-wrangler` — one binary holding the hook client, the daemon, the
  installer and the platform integration. It is named for what it wrangles
  rather than for what draws it, because nothing it does is particular to
  Zellij.
- `tmux-agent-wrangler` — the tmux client. It registers a socket sink, reads
  the state that the daemon publishes on it, and writes out every record. It
  draws no sidebar and reads no tmux topology. It takes
  `agent-wrangler-core` for the record format alone, and it never sees
  `agent-wrangler-ui`.

## Application state

Application state holds authoritative inputs and user intent. It does not hold
derived rows, placement styles, or other fragments of a previous view.

At a high level it contains:

- the latest tab report;
- the latest session layout reported by the multiplexer;
- focus observations and whether this sidebar is visible;
- the agent registry and locally pending acknowledgements;
- permissions, client status, and configuration;
- this plugin instance's stable identity;
- the selected view identity;
- render scheduling state;
- the last rendered view model, for interaction with what is on screen.

Tab reports and session layouts arrive independently and may briefly describe different
moments. They remain inputs until reconciliation; event handlers should not
immediately turn either report into rows or lifecycle decisions.

Derived values such as tree rows, notifications, focused styling, and the
selection fallback belong to the view model. Keeping them out of application
state avoids two copies of the same truth becoming inconsistent.

## Stable identity

Every domain entity has a distinct stable ID type:

- `TabId` identifies a tab for its lifetime;
- `TerminalPaneId` identifies a terminal pane;
- `PluginPaneId` identifies a plugin pane;
- an agent session ID identifies an agent session.

Zellij also exposes tab positions. A position answers where a tab is currently
drawn and is useful for ordering, labels, and APIs that explicitly require a
position. It does not identify the tab.

A reconciled tab therefore has both a stable ID and a current position. Stored
selection and row actions name the stable ID. Conversion from a tab ID to its
current position happens only at the Zellij boundary, immediately before an
operation such as switching tabs.

View identities distinguish separate appearances of the same entity. For
example, an agent in the main tree, the same agent in a grouped section, and
its notification entry are three selectable view identities that all refer to
one stable agent session.

## Reducing events

Event and pipe handlers have a narrow responsibility:

1. decode the input;
2. update the corresponding authoritative state;
3. mark the relevant state dirty;
4. emit any explicit effects that follow from the event itself.

They do not build rows, compare frames, or decide what should be highlighted.
An event reporting tabs stores tabs. An event reporting panes stores panes. An
agent message stores agent state.

The reducer is pure where possible. Host operations are represented as effects,
such as:

- refresh focus;
- answer an agent call;
- run or register the client;
- broadcast a selection;
- focus a pane or switch a tab;
- close this sidebar;
- schedule a render.

Separating decisions from execution makes it possible to test that a sequence
of events produces the correct state and effects without running Zellij.

## Reconciliation

Reconciliation is a pure operation over the latest application state. It joins
tabs, panes, agents, and focus into a normalized session snapshot keyed by
stable IDs.

The pane manifest is position-based, so reconciliation uses the current tab
report to associate each reported position with a `TabId`. Position remains
metadata on the resulting tab rather than becoming its identity.

Not every combination of reports is coherent. Reconciliation validates facts
before accepting them, including whether:

- the focused tab ID is present;
- the focused pane belongs to that tab;
- this plugin pane belongs to the tab the instance believes it is in;
- pane and tab reports agree sufficiently for lifecycle decisions.

When these facts disagree, the result records uncertainty. It must not assemble
a confident-looking state from whichever half happens to be newer.

## Focus

Focus is an observation with confidence, not a boolean attached independently
to every row.

The reconciled focus is one of:

- **confirmed**: the focused tab and pane agree with the normalized session;
- **pending**: reports disagree or a focus refresh is scheduled;
- **unknown**: there is not enough information to place the focus.

Visibility is part of focus ownership. A hidden sidebar may continue receiving
pipe messages while its Zellij session reports are frozen. It can store those
messages, but it cannot use its cached focus to conclude that the user is still
in a pane.

Only a visible instance with confirmed focus may produce effects that depend on
the user's location, such as answering a call. Pending or unknown focus may
affect conservative presentation, but must not trigger lifecycle changes or
external state changes.

Reconciliation runs on each report as it arrives, and records uncertainty while
the reports disagree. What it must not do is let that uncertainty reach the
screen: the render timer draws after a burst of reports rather than between
them, so the reading drawn is the one the burst settled on.

## Dirty state and rendering

Changing authoritative state leaves the frame on screen out of date. A render
scheduler coalesces those changes and asks Zellij to render independently of
the event that reported them.

The flow is:

1. an event updates state and decides a repaint is owed;
2. the first such decision schedules a render timer;
3. further events update the same state without scheduling more timers;
4. the timer is the only event that asks Zellij to render;
5. rendering builds a fresh view model from current state.

The application decides that a frame is stale; the adapter decides when it is
drawn. The two are separate because when to draw depends on how the host's
timer behaves, and Zellij's answer is particular: a zero-second timer is
delivered behind the events already queued, so it lands after a burst of
reports rather than between them. That is what turns the several events of one
tab switch into one frame.

Zellij may also request a render directly, for example after a resize. Rendering
must therefore always be able to build the complete view from current state and
the supplied pane size; it cannot depend on a particular event having prepared
rows first.

## View model

The sidebar view model is the exact, complete UI for one pane size. It contains:

- every visible line in screen order;
- the stable view identity associated with each selectable line;
- the action associated with that identity;
- the selection after resolving it against the visible lines;
- all placement, indicator, color, wrapping, truncation, and note data required
  to render the frame.

Tree truncation and notification fitting happen while building this model.
Consequently, a row omitted because of height or width is not considered
selectable in that view.

The same model drives:

- ANSI rendering;
- keyboard navigation;
- mouse hit testing;
- selection highlighting;
- activation.

This prevents interaction from using a larger logical tree while the user is
looking at a smaller rendered frame. The last rendered view model is retained
so an input event is interpreted against what was actually on screen when the
event occurred.

View construction is pure:

```text
view(application state, pane size) -> sidebar view model
```

The same state and size must always produce an equal view model and equivalent
terminal output.

## Effects and lifecycle decisions

View construction never performs host calls. Decisions that change Zellij or
external agent state are explicit reducer effects and are executed by the
plugin adapter.

Effects based on reconciled state require stronger evidence than presentation.
In particular:

- a call is answered only from confirmed, visible focus on its pane;
- a sidebar closes as the last pane only from settled topology;
- activating a tab resolves its stable ID to the latest position at execution
  time;
- activating an entity that no longer exists is a safe no-op.

This keeps a transient or stale snapshot from producing an irreversible action.

## Boundaries

The Zellij-facing plugin should be a thin adapter. It translates Zellij types
into application inputs, executes effects through host functions, schedules
renders, and prints the final terminal output.

The application state, reducer, reconciliation, view model, and interaction
logic belong in host-testable library code. The UI crate remains responsible
for presentation vocabulary, layout, and terminal rendering without depending
on Zellij.

Exact module names are deliberately left open. The important boundary is that
Zellij APIs do not leak into the pure state and view pipeline.

## The daemon and the hook client

Agent state lives in a daemon, one per user, started by whichever hook first
finds none running. It holds what the sessions are and nothing about where they
are shown; the sidebar is what turns a record into a row.

A hook says what it saw and exits: which agent, which event, the transcript's
path, and a named few of its own environment variables, captured verbatim. The
daemon does the reading. That keeps the hook off the critical path of the turn
it runs inside, and it is what lets the daemon do the two things a plugin never
could:

- **Watch.** Every transcript it has been told about is looked at once a second,
  and re-read when it has moved. A session that titles itself, or is given a
  colour, is drawn without any hook firing at all.
- **Reap.** A session whose process has gone is dropped. An agent killed without
  an `end` event used to leave a row nothing would ever take away.

The daemon and the hook are the same executable, and a hook starts the daemon by
running its own path, so the two can never be different builds. What can differ
is the daemon and the plugin, so a state message names the format it is written
in and a sidebar sent one it does not know says so at the top of the pane.

On Windows that executable is installed twice, as `agent-wrangler.exe` and
`agent-wranglerw.exe`. Every subcommand lives in a library and each binary is
four lines around it; the second is linked for the windows subsystem, so it is
never given a console. Windows allocates one for a console program whose parent
has none and draws a window for it, and both of the things that run this client
are exactly that: an agent running a hook, and the zellij server running what
the sidebar asks for. So it is the windowless one that the installed hooks and
the layout name, and the console one that a person runs. The two are installed
together, and a client somewhere without its twin writes hooks naming itself: a
hook that flashes a window still reports what the agent did.

The daemon has the same problem to hand on. It is started detached and so has no
console of its own, which makes every program it runs one of those given a fresh
one: the zellij pipe it holds open to a session, and the notifier it announces a
call with. Both are started through the platform module rather than built where
they are run, so what it takes to start a program without disturbing the user is
answered once for each system and cannot be forgotten at a call site.

There are two transports, and they have one shape. Each one holds a writer of
its own and a slot that holds one payload, so filling the slot returns at once
and a client whose buffer is full delays that client and no other. The record
breaks inside a payload travel as `\u{1e}`, because both transports frame by the
line. They differ in the pipe under them and in nothing else.

A zellij client is reached through one `zellij pipe` that stays open, and not
through one process per delivery. A wasm plugin cannot hold a connection, so the
daemon reaches out to it. A pipe given no payload argument reads its stdin, and
one line on that stdin is one message; a plugin answers on the same pipe, and
that answer arrives on the same process's stdout. So one child carries the state
out and the messages back, and a sidebar that answers a call costs no process at
all. The pipe process does not exit when its session dies, so the daemon kills
the child when it retires the client.

A native client holds a connection itself, so it needs no such command, and tmux
has none to offer. The client names a socket when it registers, and the daemon
binds that name and listens on it. One socket serves one session, so every
sidebar of that session reads the same one and none of them has to be elected.
The daemon holds the newest payload for each name, because a client is owed the
state the moment it registers and that is before any peer of it connects. A peer
is written the held payload as soon as it arrives. A name that a dead daemon left
behind is taken over only when nothing answers it, which is the rule that the
daemon's own socket already follows.

Liveness is one question on both: can this client still send a message? That
covers being connected, and it covers working as well. A client answers by
speaking, and the daemon gives up on a client that said nothing for ninety
seconds. Any line counts, so a client with something to report sends no separate
beat. A client with nothing to report sends `ClientMessage::Beat` every thirty
seconds, on the transport that already carries its state, so a beat costs no
process on either side.

Two weaker measures came before it, and each one had a client it could not see.
The exit status of the pipe process reads a busy multiplexer as a refusal, and
it never finds a session that lives on with no sidebar in it, which `q` creates
and FEATURES.md lists as a feature. An open connection says that the kernel kept
it, and says nothing about the process behind it: a client whose reader thread
died holds one open for as long as it lives. Both of those clients pass the old
question and fail this one.

A peer that disconnects is not a client that has gone, because the sidebars of a
session come and go while the session stays. So losing a peer is not a second
rule. A client that has really gone stops beating, and silence retires it.

The ninety seconds is three beats. A client that is retired goes deaf for good,
because it registers once, so the wait must cover a sidebar restarting and a
daemon restarting with its clients connecting again. A register starts the clock
as well, which gives a client that has just arrived the whole ninety seconds to
connect and speak for itself.

A zellij sidebar does not choose its own interval. It writes only while it
handles a message from the pipe, so the daemon's own beat sets the cadence, and
that beat is thirty seconds when nothing is happening. Every sidebar of a session
answers on the same pipe. The daemon holds one clock for the session and never
asks which sidebar spoke, so no sidebar has to be elected to speak for the rest.

A plugin can only write on a pipe while it is handling a message from that pipe.
Zellij holds anything written at any other moment and hands it over on the next
message, in order and losing nothing. A sidebar answers a call when the focus
moves, which is not a message, so its answer waits for the daemon to write
again — and the daemon writes only when something changed, which the answer is.
The delivery thread therefore wakes whether or not anything is owed, and writes
one empty line down each held pipe. That line is a message to zellij and nothing
to a sidebar, which reads no state in it and draws nothing again. A publish
serves the same purpose when there is one to make.

The beat has two rates, because each write costs a line in zellij's log. While
an agent waits for the user it is a second, which is what makes an answered call
stop being drawn in the other tabs at once. The rest of the time it is thirty
seconds, which keeps the transport warm and costs two lines a minute. The
daemon does not ask which session holds the call: it knows that somebody is
calling, and a machine has few sessions and short calls.

Records survive the daemon being restarted, but only those naming a process
still running: a live agent says so again on its next event of any kind, where a
dead one would otherwise be drawn for good.

State is kept under `$XDG_STATE_HOME/agent-wrangler` (`%LOCALAPPDATA%` on
Windows). The daemon is reached over a local socket, which is a unix socket on
unix and a named pipe on Windows.

The agents of a session are known to every sidebar in it, and a sidebar opening
in a new tab asks the others for what they have. Nothing survives every sidebar
being closed at once.

`agent-wrangler monitor` writes one line of JSON per message and nothing while
nothing is arriving. Each says which way the message went, and `told` says why
an arriving message is worth reading: whether it changed anything, since that
and not the arrival is what owes the clients a delivery. One record is written
for each state that goes out. A delivery is a write and not a process run, so
there is nothing to say afterwards about one that landed; only a failure is
worth a second record, and no count of those retires anybody. One record is
written for each beat, so a monitor shows the circle turning while nothing else
happens, and one for every client that the daemon gives up on, so a feed that
stopped has an explanation.

## The tmux client

`tmux-agent-wrangler` reads the shape of one tmux session, holds an
`Application` of the shared crate, and draws the sidebar into its own pane. It
takes the same three shared crates that the zellij plugin takes, and it adapts
at the same boundary.

One thread owns the application and draws. Every other thread sends it one kind
of event and touches no state:

```
socket reader   ->  a state payload, or the reader gave up
control client  ->  something moved, an answer, or the server went
change ticker   ->  something moved                (the fallback feed)
input reader    ->  the user asked to stop
child runner    ->  a program that an effect started has finished
                        |
                        v
                 std::sync::mpsc
                        |
                        v
      the drawing thread: Application::reduce, then Terminal::draw
```

Nothing there needs a runtime, an async crate or a signal handler.

The drawing goes through ratatui. The `Sidebar` widget fills a buffer for both
clients. The zellij plugin turns that buffer into bytes with `frame_to_ansi`,
because printing is all a plugin can do. The tmux client hands the same widget to
a ratatui `Terminal`, which holds the new frame beside the one before it and
writes only the cells that differ.

The tmux client therefore draws after every event, and it never decides whether
to draw. A resize explains that rule. Tmux reports a resize to no program, and a
resize changes nothing in the application. A client that drew only on a change
of the application would hold the frame of the old width until something else
moved.
`Terminal::draw` reads the size of the pane every time, so the frame follows the
pane.

The client owns its pane while it runs: the alternate screen, raw mode on,
cursor hidden. The alternate screen has no history behind it, which suits a
program that draws a whole pane at a time and has nothing to scroll back
through. On the normal screen a host keeps a history for such a pane anyway, and
one frame too tall would start filling it. Leaving that screen also gives the
pane back as the client found it, rather than leaving the last frame behind. Raw mode
stops the pane echoing a keystroke over the drawing, and it also stops Ctrl-C
raising an interrupt. So the client reads its input and answers `q` and Ctrl-C
itself. `ratatui::try_init` takes the pane and installs the panic hook. The
sidebar gives the pane back as it drops, and the hook gives it back when a panic
ends the program. A panic on another thread does not unwind the drawing thread,
so only the hook covers that case.

### How a change reaches the sidebar

A control mode client, `tmux -C attach`, sends a line for every change in the
session and runs a command written on its standard input. Every notification
means one thing here: ask again. Nothing decodes the layout string in
`%layout-change`, which is a checksum and a nested geometry grammar.

The question goes down that same client, as one line of four commands joined by
semicolons. Each of them gets a reply block of its own, and their bodies joined
are exactly what those commands write when they run as a child process. So one
reader parses the answer whichever transport carried it.

A control client is an attached client, and tmux counts an attached client when
it sizes the windows of a session. This one is 80 by 24, because its output is a
pipe. Under the default `window-size latest` the windows of the user snap to
that size the moment a sidebar starts. A control client also receives every byte
that every pane writes. Two flags settle both:

```
refresh-client -f no-output,ignore-size
```

A server that does not know a flag accepts the command and does nothing with it,
so an error check finds nothing. Psmux does exactly that. The handshake
therefore asks the server to name its flags back with `#{client_flags}`, and the
sidebar keeps the control client only when the answer names `no-output`. A
server that fails the check is left, and a timer asks every half second instead.
That is a capability check and not a `cfg`, so psmux works today and gets the
faster feed with no change here on the day it grows the flag.

The timer runs whether or not a control client does. A control client that dies
leaves no gap in the feed, and a tick that arrives while a question is
unanswered costs one line on a pipe.

Two rules about writing a tmux command line. Tmux parses these itself, so `#`
starts a comment there and every format is quoted. Tmux also reads an argument
that starts with a dash as a flag, so every marker that this client prints
starts with a letter.

### What tmux is asked, and what is done with the answer

Two questions and not one. A window name and a pane title are both free text and
either can hold a tab, so one query carrying both would need a repair when a
split came out with the wrong number of fields. Two queries each put their one
free text field last, and a split into four parts is then exact.

A window is identified by `#{window_id}` and a pane by `#{pane_id}`, which are
stable. The order in the list becomes `TabPosition`, because the shared code
joins a tab report to a session layout on it. The number that tmux calls the
window becomes `displayed_index`, and the row draws that number. A user sets
`base-index` to any value, and a closed window leaves a gap, so the order and the
number are two facts.

The pane title is not `#{pane_title}` alone. That holds what a program set with
an escape sequence, and a program such as `sleep` sets none, so tmux answers the
host name. The format asks whether the title is still the host name and names
the running program when it is. Zellij falls back to the command in the same
way.

The pane that the client runs in is reported as the sidebar pane of its window,
and it is left out of the content panes. Tmux parks no pane, so every pane is on
screen. Tmux runs no plugin, so no other kind of pane holds the focus.

### The socket, and what proves the transport

The client also registers a socket sink and reads the agent state on it. That
half proves the transport, from a hook in a pane to a record in the registry.

The binary must find its own session before it can name a socket. The first
field of `$TMUX` is the server, which is a path on unix and the name of a pipe
on Windows. Nothing reads that field for meaning. `TMUX_PANE` is the pane, and
the session comes from the pane through `tmux display-message`. The third field
of `$TMUX` names the session that the process started in, and that field goes
stale when a window moves to another session. `TMUX_PANE` stays true.

The socket name carries a hash of the whole server string and the session after
it. The hash tells apart two servers whose sockets share a basename in different
directories, and it works for a server that is a named pipe. The session keeps
the name useful to a person who lists the sockets. The hash is FNV-1a and never
`DefaultHasher`, because the standard library does not specify what
`DefaultHasher` gives back. Every sidebar of one session must derive one name.
Two sidebars that derive two names read two sockets and never agree.

A session id is a dollar sign and one or more digits. `TmuxSessionId::new`
rejects everything else, so `SocketName::new` returns no error and runs no check.
Every character of a name is then an ASCII letter, a digit, a hyphen or a dot,
which the namespace accepts. A wider `Session` needs a check in the name.

The client registers before it connects, on every round and not only on the
first. A daemon that gives up on a client releases the socket name and drops the
client record together. Only a new registration makes the daemon bind the name
again. A client that only reconnects finds nothing to connect to, and stays
deaf.

The connect retries, because the daemon binds the name while it handles the
registration. The wait is bounded at two seconds. A daemon that never binds the
name is a fault to report rather than a thing to wait for. The bound must stay
well inside the ninety seconds that the daemon waits before it gives up on a
silent client. A slower reconnect costs the registration that it tries to
restore.

A stream that ends says that the daemon went, and the client goes round again. A
payload that the sidebar refuses says that the thread which draws has stopped,
and the client stops.
These are two outcomes and not one, and the type says which. The kind of an
error names what went wrong and never which end it went wrong at.

The two systems spell the end of a stream differently. A unix peer's read
answers zero after a shutdown, and a Windows client's read fails after a
`DisconnectNamedPipe`. A program that waits for zero alone works on unix and
waits for ever on Windows. Both spellings take one arm.

The crate is built on every system and not on unix alone. Tmux does not run on
Windows, but psmux does, and psmux ships a program called `tmux`. So the crate
holds no `cfg` for a system, builds no path, and names no directory. Crossterm
holds the difference between the systems for raw mode and for the size of a
pane, and the crate takes it with default features off: the `events` feature is
what pulls a signal handler, and this crate installs none.

A green Windows job compiles it. Nothing in the build runs `tmux`, so the
Windows path is not proven from end to end.

A record is kept only when the pane that it names belongs to this tmux server.
The pane id alone is not enough, because two servers number their panes from the
same counter and `%1` names a pane on each.

## Testing

The architecture should support tests at three levels:

- reducer tests feed event sequences into state and assert state plus effects;
- view-model tests assert the exact visible and selectable lines for a state and
  pane size;
- end-to-end tests verify the adapter against a real Zellij session.

Reducer tests should permute tab, pane, focus, visibility, and agent-message
ordering. View tests should cover resizing, truncated trees, notifications that
do not fit, and shared selections across sidebars of different sizes. Stable-ID
tests should close or insert preceding tabs and verify that selection and
activation still refer to the same tab.

End-to-end tests remain valuable for host behavior that cannot be represented
by the library, but correctness of event ordering and rendering should not rely
on them alone.

## Migration status

Every migration step is complete. Multiplexer-neutral application state,
reducer inputs, decisions, effects, and reconciliation live in
`agent-wrangler-sidebar`; the Zellij plugin translates host reports and executes
the resulting effects. Pane and tab row keys are opaque and stable across the
portable boundary. Tab-switch effects carry stable IDs, which Zellij resolves
to the latest reported position only when it executes its positional host API.
The complete last rendered view now owns the exact visible interaction map,
resolved selection, and stable-ID actions used by navigation, clicks,
highlighting, and activation. Derived rows, notifications, and focus styling
are rebuilt from authoritative state for every render rather than cached in the
application. Focus reconciliation is visibility-aware and distinguishes
confirmed, pending, and unknown observations; rendering applies active,
focused, and sidebar-selection styling only from one confirmed snapshot. Call
answering, hook installation ownership, remembered focus, and automatic sidebar
closure are decided together only from a visible, confirmed reconciliation;
pending or stale observations can update facts but cannot produce those effects.

Repaint decisions are coalesced through a render schedule in the adapter: a
decision that the frame is stale asks the host for a timer, and only the timer
draws. Measured at ten tabs, a tab switch drew four frames across the two
sidebars it involves and now draws two, one each, and the number of frames no
longer follows the number of events the host sends.

The migration is complete. The rest of the document is the architecture as
built.

## Migration direction

The architecture can be introduced incrementally:

1. extract application state, reducer decisions, and effects from the plugin
   binary;
2. replace positional tab keys with stable tab IDs;
3. make the exact rendered view model the source of all interaction;
4. introduce visibility-aware, validated focus reconciliation;
5. move call answering and sidebar lifecycle changes behind confirmed effects;
6. coalesce dirty state through the render scheduler.

Each step should preserve the existing terminal presentation while reducing the
amount of behavior that depends on event timing.
