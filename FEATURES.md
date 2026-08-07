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
- [ ] ~~Configurable width, clamped to a min and a max~~ (the layout's, not the
      sidebar's)
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
- [x] Sections mode: the tree, then a block per agent, the same rows regrouped
- [x] A pane's title follows the command and the directory running in it
- [x] Agent labels update live
- [x] Agent label from the working-directory basename
- [x] Agent label from the session title, when it has one
- [x] Teammates labelled `@name - title`

A session's title is in neither agent's hook body, so the client reads it off
disk: Claude's from the end of the transcript the body names, Copilot's from the
workspace file it keeps per session. That is also what makes a label live
without anything watching anything - an agent fires hooks throughout its turn,
and every one of them carries the whole record, so a title taken on mid-session
arrives on the next event of any kind.

What the client reports is what a session is *called by* rather than what its
row says: the directory, the title, and the teammate name. Composing those into
a label is the sidebar's, which is why the label option changes every row at
once with no agent reporting itself again.

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
- [x] `Enter` on a notification opens it
- [ ] The selection falls back sanely when the row it was on is gone

## The agent registry

- [x] A hook client the agents invoke, taking the payload on stdin
- [x] `start` / `end` / `working` / `needsAttention` / `error`, snake and camel payloads
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

- [x] Working and attention state driven by the agent hooks
- [x] Attention clears when you focus the agent's pane

Whose turn it is rides on the record, so a sidebar that opens later learns it
with everything else. Nothing is sent between sidebars to clear attention:
arriving at an agent's pane is not an event of its own, it is whichever change
moved the focus, and every sidebar reads that the same way and reaches the same
answer.

## Notifications

- [ ] ~~Terminal bell when an agent needs attention~~ (dropped)
- [x] Desktop notification for the same event
- [x] Notification area at the foot: newest first, one entry per session, capped
- [x] An entry is a title over its wrapped message, admitted only if it fits whole
- [x] The area never grows past a quarter of the pane
- [x] Opening an entry jumps to the pane the agent is in now
- [x] Opening dismisses every entry naming that pane, and only those
- [x] An entry clears when you focus the pane it points at
- [x] An event raised by the pane you are in never appears
- [x] Each event raises its entry exactly once

The area is not kept, it is read off the registry: the entries are the agents
calling for the user, newest call first, each described by the tab holding its
pane and its own label. That makes every one of these a consequence rather than
a rule. An entry is one per session because an agent is; opening one goes where
the agent is now because the pane is read at the time; arriving answers the call,
so opening dismisses exactly the calls raised from the pane landed on, and a call
raised by the pane already focused is answered in the same pass that recorded it
and never draws.

The desktop notification is the one thing here that leaves the terminal, and it
leaves it by running a command rather than by writing an escape: zellij passes
none through. It carries what an entry carries, the agent's name over where it
is, and it is raised for every call whichever pane is focused, because being in
a pane says nothing about whether the terminal is on screen at all. The area
answers to focus and the notification does not, and that is the difference
between the two.

The bell was dropped. A plugin cannot ring one, and reaching a terminal from the
hook client turned out to need a search up the process tree for an ancestor
still attached to one, which is far more machinery than a beep is worth.

## Hook installation

- [x] Install the hook invocations into each agent's config from a manifest
- [x] Merge into Claude's shared settings without touching other hooks, with a backup
- [x] Write Copilot's dedicated config outright
- [x] Idempotent, with `--uninstall`, upgrading a command written under the other name
- [x] Install on load, behind an option

A hook command is claimed only when the program it runs is named
`zellij-wrangler`, or `wrangler` from a path holding `zellij-agent-wrangler`.
Matching the program rather than a word in the line is what lets another
installer's hooks sit in the same file untouched. The command is read back with
the quoting it was written with, so a path with a space in it is recognised
rather than installed again beside itself.

## Options

- [x] Every user-facing option the original exposes that means anything here, as
      plugin configuration: label mode, sections, turn state, notification area,
      desktop notification, installing the hooks on load

Width is the layout's. A plugin cannot size its own pane, and the pane the
sidebar is in is declared where every other pane of the tab is declared, so a
width option would be a second place to say the same thing and the losing one.
The two the original has for clamping a drag follow it.

A desktop notification is raised by running a command, since zellij passes no
escape through to the terminal. It is raised by the one sidebar in the tab the
user is in, which is how every sidebar hearing the same call produces one
notification; the same rule installs the hooks once. Both are the only things
the sidebar asks to run commands for, so a sidebar with neither turned on never
asks for the permission.

## Distribution

- [x] A published wasm and a documented one-line install
- [x] Keep the hook client and the plugin in step across an update
- [x] Split the crate so the hook client does not link `zellij-tile`

The client reads its stdin and runs one command, and used to link 250 crates to
do it: the library reads zellij's types in one module, which off wasm brings in
curl, openssl and the rest of `zellij-utils`. That module is behind the `plugin`
feature and the client is built without it, which leaves it on `serde_json`
alone: 9 crates.

Both halves are released together and named for the same tag, and the install
script prints the layout block pinned to the release it just installed. Naming
the wasm for its version is not decoration: zellij caches a downloaded plugin
under the last segment of its url and returns early if that file already exists,
so two releases both called `zellij-agent-wrangler.wasm` would be one file and
an update would never arrive.

Every record carries the format it is written in, so a client of another version
is reported rather than ignored: without it an out-of-date client's records
would simply fail to parse, and the only symptom would be agents that never
appear.

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
- [x] **5. Turn state.** The rest of the hook events. Working while it works,
      attention when it wants you, cleared when you focus its pane.
- [x] **6. Notifications.** The area fills on attention, opening an entry lands
      on the agent's pane and dismisses the right entries. Bell and desktop
      notification beside it.
- [x] **7. The rest.** Options, sections mode, label modes, hook install,
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
- [ ] **Phase 3, in parallel.** Notifications; options and
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
