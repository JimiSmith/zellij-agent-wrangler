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

Liveness is answered differently on the two. A zellij client is given up on after
three refused deliveries, because the exit status of the pipe process is the only
signal there is. A socket sink is given up on when it has had no peer for thirty
seconds. The daemon reads its own listener, so that question costs nothing, and
it is the better measure: the exit status reads a busy multiplexer as a refusal,
and it never finds a session that lives on with no sidebar in it. A peer that
disconnects is not a client that has gone, because the sidebars of a session come
and go while the session stays.

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
worth a second record, and enough of those in a row retires a zellij client. A
record is also written for every client that the daemon gives up on, whichever
of the two rules ended it, so a feed that stopped has an explanation.

## The tmux client

`tmux-agent-wrangler` is the socket sink's first reader. It registers, connects,
and writes out every record that arrives. It draws no sidebar and it reads no
tmux topology. What it proves is the transport, from a hook in a pane to a line
on a stream.

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

A session id is a dollar sign and one or more digits. `place::Session` refuses
everything else, and that refusal is what makes the socket name infallible.
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
well inside the time that the daemon gives a sink with no peer. A slower
reconnect costs the registration that it tries to restore.

A stream that ends says that the daemon went, and the client goes round again. A
write that fails says that the reader of the output went, and the client stops.
These are two outcomes and not one, and the type says which. The kind of an
error names what went wrong and never which end it went wrong at.

The two systems spell the end of a stream differently. A unix peer's read
answers zero after a shutdown, and a Windows client's read fails after a
`DisconnectNamedPipe`. A program that waits for zero alone works on unix and
waits for ever on Windows. Both spellings take one arm.

The crate is built on every system and not on unix alone. Tmux does not run on
Windows, but psmux does, and psmux ships a program called `tmux`. So the crate
holds no `cfg` for a system, builds no path, and names no directory. A green
Windows job compiles it. Nothing in the build runs `tmux`, so the Windows path
is not proven from end to end.

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
