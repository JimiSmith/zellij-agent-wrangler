# zellij-agent-wrangler

A zellij sidebar pane listing tabs, their panes as a tree, and the agent
sessions running in them.

**This is a port in progress.** The tree and the agent rows are live; turn
state, notifications and the options are not. `FEATURES.md` is the list, and
`PROGRESS.md` is what the design rests on.

```
▌ 1: wrangler
  ├─ 1:   nvim
▌ ├─ 2: 󱙺  wrangler
  └─ 3:   cargo watch
  2: notes
  ├─ 1:   nvim
  └─ 2: 󱙺  docs
  3: infra
  ├─ 1:   ssh prod-1
  └─ 2:   k9s
```

## Running it

```bash
rustup target add wasm32-wasip1
./dev.sh                 # builds both halves, then opens a session with the sidebar in every tab
cargo test               # everything that does not call zellij, on the host target
```

Two things are built: the plugin, which is the crate without its `native`
feature, and `wrangler`, the hook client the agents invoke. `dev.sh` prints the
client's path.

## Agent rows

An agent appears in the tree once its own lifecycle hooks call the client, which
reports the pane it was invoked in. Installing those hooks automatically is not
built yet, so add them by hand, with the path `dev.sh` printed:

```json
{
  "hooks": {
    "SessionStart": [
      { "hooks": [{ "type": "command", "command": "/abs/path/to/wrangler hook claude start" }] }
    ],
    "SessionEnd": [
      { "hooks": [{ "type": "command", "command": "/abs/path/to/wrangler hook claude end" }] }
    ]
  }
}
```

In `~/.claude/settings.json` that covers every agent you start anywhere; the
client does nothing at all outside zellij, so sessions elsewhere are unaffected.
Start an agent in a pane and that pane's row becomes the agent's, labelled with
the directory it is working in. A pane running two agents contributes a row
each.

The agents of a session are known to every sidebar in it, and a sidebar opening
in a new tab asks the others for what they have. Nothing survives every sidebar
being closed at once.

Each run kills the `wrangler-proto` session it opens last time, because a
session holds the wasm it loaded at startup and attaching would run the build
before last.

**The first run comes up blank.** The sidebar needs zellij's
`ReadApplicationState` and `ChangeApplicationState`, and zellij asks by drawing
over the plugin's pane, but nothing paints the question until something else
forces a redraw, and a plugin is not rendered at all while its request is
pending. Focus the sidebar (`Ctrl+g`, then `Ctrl+p` and `←`) and press `y`.
Zellij caches the answer against the wasm's path in
`~/.cache/zellij/permissions.kdl`, so this is once per machine rather than once
per run, and the path does not change when the wasm is rebuilt. Each tab that
existed before the answer asks separately, since every tab's sidebar is its own
plugin instance; tabs opened afterwards find the answer cached. See
[zellij#4749](https://github.com/zellij-org/zellij/issues/4749).

Keys reach the plugin whenever its pane has focus. Zellij starts every session
in locked mode, which turns off zellij's own bindings and passes keystrokes
through to the focused pane, so the sidebar works while locked; `Ctrl+g`
unlocks, which is what you need to move focus onto the sidebar (`Ctrl+p` then an
arrow) or to quit (`Ctrl+q`).

`j`/`k` and the arrow keys move the selection; a click goes to the row it lands
on, as does `Enter`. `q` turns the sidebar off for the whole session.

The sidebars of a session share their selection, so they read as one sidebar
that follows you.

Every tab has one, including tabs opened later: the layout declares a
`new_tab_template` beside its `default_tab_template`, with a plain pane where
`children` would be.

Nerd Font glyphs are used for the pane and agent icons, so the terminal needs a
patched font to draw them.

## What it demonstrates

- **The paint.** `render.rs` chooses every glyph that is not the literal name of
  a thing: the gutter, the tree branches, the index prefix, the kind icon, and
  the styling. A row is drawn as styled pieces rather than one styled line,
  which is what lets an agent's color sit on its icon alone.
- **Two regions.** The tree fills the pane; the notification area is pinned to
  the foot, capped at a quarter of it, and admits an entry only if it fits
  whole, so a title never appears over a cut-off message.
- **Input.** Key and mouse events arrive for the plugin's own pane with no
  permission grant, so the prototype prompts for nothing on load.
- **A still pane.** Turn state is two static glyphs, `●` for the agent that
  wants you and `○` for the one still going, and the plugin subscribes to no
  clock. It repaints only when a key or a click changes what it would draw, and
  costs nothing between.

## Layout

- `model.rs` — the row vocabulary: what a row is, where it sits, what its turn
  state is.
- `render.rs` — the line drawn for a row, and the styling of its pieces.
- `session.rs` — the shape of the session, read out of what zellij reports. The
  only place zellij's types meet the sidebar's.
- `tree.rs` — the rows a session is drawn as.
- `agents.rs` — the agent sessions, their wire format, and the panes they sit in.
- `payload.rs` — reading an agent's hook body. Native only.
- `main.rs` — the plugin: the two regions, the nav order over them, and the
  event handling.
- `wrangler.rs` — the hook client.
