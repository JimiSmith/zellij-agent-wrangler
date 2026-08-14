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

## Application state

Application state holds authoritative inputs and user intent. It does not hold
derived rows, placement styles, or other fragments of a previous view.

At a high level it contains:

- the latest tab report;
- the latest pane report;
- focus observations and whether this sidebar is visible;
- the agent registry and locally pending acknowledgements;
- permissions, client status, and configuration;
- this plugin instance's stable identity;
- the selected view identity;
- render scheduling state;
- the last rendered view model, for interaction with what is on screen.

Tab and pane reports arrive independently and may briefly describe different
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

Focus reconciliation should happen after related session events have had a
chance to settle. A scheduled timer can coalesce adjacent tab and pane reports,
refresh focus once, and then request a render. This also keeps host queries out
of pipe handlers that cannot safely make synchronous round trips.

## Dirty state and rendering

Changing authoritative state marks the application dirty. A render scheduler
coalesces changes and asks Zellij to render independently of the event that
reported them.

The intended flow is:

1. an event updates state and marks it dirty;
2. the first dirty transition schedules a render timer;
3. further events update the same state without scheduling more timers;
4. the timer performs any required reconciliation work and requests rendering;
5. rendering builds a fresh view model and clears the dirty state.

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

Migration step 1 is complete. Multiplexer-neutral application state, reducer
inputs, decisions, effects, and reconciliation live in `agent-wrangler-sidebar`;
the Zellij plugin translates host reports and executes the resulting effects.
Pane IDs are also opaque and stable across the portable boundary, and focus
already distinguishes stable tab IDs from tab positions.

The rest of the document remains the target state. In particular, tab row keys
are still positional, the last exact rendered view is not yet the source of all
interaction, focus has no visibility or confidence state, lifecycle effects are
not gated on confirmed focus/topology, and repaint decisions are not yet
coalesced through a dirty-state scheduler.

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
