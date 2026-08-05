# Features to port

Everything tmux-agent-wrangler does, as a list of work. A checked box is done in
this repo; the drawing ones are complete as code, drawn from hardcoded rows.

## Sidebar lifecycle

- [ ] One sidebar pane per tab, never a single shared pane
- [x] A tab opened later gets a sidebar too (`default_tab_template`)
- [ ] Toggle the sidebar for the session, off a key binding
- [ ] Focus this tab's sidebar, off a key binding
- [ ] `q` closes the sidebar
- [ ] The sidebar exits when its tab has no real panes left
- [ ] A sidebar draws the session its own tab belongs to
- [ ] Configurable width, clamped to a min and a max
- [ ] A resize of one sidebar is adopted by the others drawing that session
- [ ] The sidebars drawing one session share a selection

## The tree

- [ ] Tabs, with their panes as children
- [ ] A pane running an agent is drawn as that agent instead of as a pane
- [ ] A pane hosting two agents contributes two rows
- [ ] Sections mode: the tree, then a block per agent, the same rows regrouped
- [ ] Pane titles and agent labels update live
- [ ] Agent label from the session title, or the working-directory basename
- [ ] Teammates labelled `@name - title`

## Drawing

- [x] Gutter marking the tab you are in and the pane you are in
- [x] Tree branches, index prefixes, heading spacing and case
- [x] Kind icons: pane, agent
- [x] A child's color rides on its icon alone; a tab row colors its whole line
- [x] Intensity says placement: bold where you are, dim for a tab you are not in
- [x] The selection bar spans the width and drops the color and dimming it covers
- [x] Rows fitted to the pane width, padded or cut from the right
- [ ] Agent color from the session's own color, matched to the Claude theme
- [ ] Pane color from the pane's border color

## Navigation

- [x] `j`/`k` and the arrows move the selection, tree and notification area as one order
- [x] A click selects the row it lands on
- [ ] `Enter` focuses the selected tab, pane or agent
- [ ] `Enter` on a notification opens it
- [ ] The selection falls back sanely when the row it was on is gone

## The agent registry

- [ ] A hook client the agents invoke, taking the payload on stdin
- [ ] `start` / `end` / `working` / `needsAttention` / `error`, snake and camel payloads
- [ ] Sessions recorded on disk, pruned when the pane or the process goes
- [ ] A recorded pane counts only when the agent's pid descends from the pane's
- [ ] A pane-less session is matched by title against each pane's live title
- [ ] A session is filed under every tab showing it, and dropped if shown nowhere
- [ ] Title collisions broken by the recorded pane, then the cwd
- [ ] Legacy two-field records still read

## Turn state

- [ ] Working and attention state driven by the agent hooks
- [ ] Attention clears when you focus the agent's pane

## Notifications

- [ ] Terminal bell when an agent needs attention
- [ ] Desktop notification for the same event
- [ ] Notification area at the foot: newest first, one entry per session, capped
- [x] An entry is a title over its wrapped message, admitted only if it fits whole
- [x] The area never grows past a quarter of the pane
- [ ] Opening an entry jumps to the pane the agent is in now
- [ ] Opening dismisses every entry naming that pane, and only those
- [ ] An entry clears when you focus the pane it points at
- [ ] An event raised by the pane you are in never appears
- [ ] Each event fires the bell, the notification and the entry exactly once

## Hook installation

- [ ] Install the hook invocations into each agent's config from a manifest
- [ ] Merge into Claude's shared settings without touching other hooks, with a backup
- [ ] Write Copilot's dedicated config outright
- [ ] Idempotent, with `--uninstall`, upgrading a command written by an older release
- [ ] Install on load, behind an option

## Options

- [ ] Every user-facing option the original exposes, as plugin configuration:
      width, min/max width, width sync, label mode, sections, hook progress,
      bell, desktop notification, notification area, auto-install hooks

## Distribution

- [ ] A published wasm and a documented one-line install
- [ ] Keep the hook client and the plugin in step across an update

## Blocked

- OSC 9;4 progress indicators: zellij reports no per-pane progress state
- Desktop notification by escape: zellij passes no OSC through to the terminal,
  so the event has to reach the user some other way
- Animated spinner: a plugin has no partial update, so any animation reprints
  the whole pane at its frame rate
