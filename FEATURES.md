# Features to port

Everything tmux-agent-wrangler does, as a list of work. A checked box is done in
this repo.

## Sidebar lifecycle

- [x] One sidebar pane per tab, never a single shared pane
- [x] A tab opened later gets a sidebar too (`default_tab_template`)
- [x] Turn the sidebar off for the session
- [ ] Turn it back on again, off a key binding
- [ ] Focus this tab's sidebar, off a key binding
- [x] `q` closes the sidebar
- [x] The sidebar exits when its tab has no real panes left
- [ ] A sidebar draws the session its own tab belongs to
- [ ] Configurable width, clamped to a min and a max
- [ ] ~~A resize of one sidebar is adopted by the others~~ (dropped)
- [x] The sidebars drawing one session share a selection

Sidebars reach each other by broadcasting: a message naming this plugin's url
reaches no running instance and launches another one instead, while an
unaddressed message reaches every plugin there is, sender included. It needs the
`MessageAndLaunchOtherPlugins` permission, and without it the send is dropped
silently.

A layout gives every tab a sidebar, runtime-created tabs included, and carries
the tab bar and the status bar beside it. `children` has to be a direct child of
`default_tab_template`, so a `new_tab_template` is declared alongside it with a
plain pane where `children` would be: a tab built at runtime needs a pane, not a
placeholder.

## The tree

- [x] Tabs, with their panes as children
- [x] A pane running an agent is drawn as that agent instead of as a pane
- [x] A pane hosting two agents contributes two rows
- [ ] Sections mode: the tree, then a block per agent, the same rows regrouped
- [x] A pane's title follows the command and the directory running in it
- [ ] Agent labels update live
- [x] Agent label from the working-directory basename
- [ ] Agent label from the session title, when it has one
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
- [x] A click goes to the row it lands on
- [x] `Enter` focuses the selected tab, pane or agent
- [x] An agent's row takes you to the pane the agent is in
- [ ] `Enter` on a notification opens it
- [ ] The selection falls back sanely when the row it was on is gone

## The agent registry

- [x] A hook client the agents invoke, taking the payload on stdin
- [x] `start` / `end`, snake and camel payloads
- [ ] `working` / `needsAttention` / `error`
- [x] Sessions held by every sidebar, and asked for by one that starts later
- [ ] Sessions surviving a session with no sidebar running at all
- [ ] A recorded pane counts only when the agent's pid descends from the pane's
- [ ] A pane-less session is matched by title against each pane's live title
- [ ] A session is filed under every tab showing it, and dropped if shown nowhere
- [ ] Title collisions broken by the recorded pane, then the cwd

The hook client reports the pane it was invoked in from `$ZELLIJ_PANE_ID`, which
zellij sets on every terminal pane it spawns and every process started in one
inherits. That number is the same one the plugin sees as a terminal pane's `id`,
so the two ends agree on a pane without anything in between.

It reaches the sidebars through `zellij pipe`, which is delivered to every
running plugin of the session it was run from whatever tab that plugin is in. A
record whose pane is not on screen is held but drawn nowhere, so an agent in a
pane that has gone leaves no row behind.

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
- [ ] Split the crate so the hook client does not link `zellij-tile`

The client reads its stdin and runs one command, and links 250 crates to do it:
the library reads zellij's types in one module, which off wasm brings in curl,
openssl, rusqlite and the rest of `zellij-utils`. Making `zellij-tile` optional
behind a feature, with that module gated on it, leaves the client on
`serde_json` alone. The crate builds for `x86_64-pc-windows-gnu` either way.

## Checkpoints

Each one is a build you can run and judge by eye. Nothing here needs
instrumenting to tell whether it works.

- [x] **1. The live tree.** Real tabs and panes, drawn read-only. Split, close,
      rename, switch tabs, move focus: the tree matches and the gutter follows.
      Also answers whether one instance sees the whole session's panes, which is
      what decides the topology below.
- [x] **2. Activation.** `Enter` and a click focus the real tab and pane. From
      here it is a usable pane switcher with no agent support at all.
- [x] **3. Lifecycle.** Leaving an empty tab, `q` turning the sidebar off for
      the session, a sidebar opening itself in a tab that has none, and a shared
      selection. Width sync was dropped.
- [x] **4. Agent rows.** `start` and `end` only. Launch an agent in a pane and
      watch its row appear with its label, then go when the agent exits.
- [ ] **5. Turn state.** The rest of the hook events. Working while it works,
      attention when it wants you, cleared when you focus its pane.
- [ ] **6. Notifications.** The area fills on attention, opening an entry lands
      on the agent's pane and dismisses the right entries. Bell and desktop
      notification beside it.
- [ ] **7. The rest.** Options, sections mode, label modes, hook install,
      distribution.

Checkpoints 1 to 3 need no agent running, so they can be tested in any session.
Checkpoint 4 is the first that needs the native binary to exist.

## Porting steps

The split is by seam rather than by feature: each piece owns its modules and
meets the others at one agreed type.

- [ ] **Phase 0.** Freeze the types everything else compiles against: the
      command enum, the resolved view the tree is built from, the pipe message
      schema, the permission set. Serial, and nothing fans out before it.
- [ ] **Phase 1, three pieces in parallel.**
  - [ ] *Tree*: the manifest and the placements to a row tree. Owns `rows`.
  - [ ] *Assoc*: records, pid ancestry and title matching to placements. Owns
        `assoc` and the registry read side. Pure, and the original's fixtures
        come across with it.
  - [ ] *Hooks*: the native side. The hook binary, the relay that orders and
        coalesces events into one pipe per session, and the installer. Shares no
        files with the other two and needs no zellij to test.
- [ ] **Phase 2.** Integration, serial: the reducer, the live subscriptions,
      activation, the lifecycle. One owner, because this is where the three meet
      and where the plugin entry point is rewritten.
- [ ] **Phase 3, in parallel.** Notifications and bell; options and
      configuration; publishing the wasm and keeping it in step with the binary.

Two rules that keep the parallel work parallel: the plugin entry point belongs
to Phase 2 and nobody touches it before then, and every module is a pure
function of values a test can construct, so no piece waits on a running session
to verify itself.

## Blocked

- OSC 9;4 progress indicators: zellij reports no per-pane progress state
- Desktop notification by escape: zellij passes no OSC through to the terminal,
  so the event has to reach the user some other way
- Animated spinner: a plugin has no partial update, so any animation reprints
  the whole pane at its frame rate
