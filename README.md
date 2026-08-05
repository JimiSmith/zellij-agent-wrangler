# zellij-agent-wrangler

A zellij sidebar pane listing tabs, their panes as a tree, and the agent
sessions running in them.

**This is a prototype.** The rows are hardcoded. It draws one fixed arrangement
and lets you move through it, which settles how the sidebar looks and how it
takes input before anything resolves live state.

```
▌ 1: wrangler
  ├─ 0:   nvim
▌ ├─ 1: 󱙺  claude · wrangler   ○
  └─ 2:   cargo watch
  2: notes
  ├─ 0:   nvim
  └─ 1: 󱙺  copilot · docs      ●
  3: infra
  ├─ 0:   ssh prod-1
  └─ 1:   k9s

 NOTIFICATIONS
 󱙺  copilot                    ●
    Permission required to run
    cargo test --release
```

## Running it

```bash
rustup target add wasm32-wasip1
./dev.sh                 # builds, then opens a session with the sidebar in every tab
cargo test               # the paint's unit tests, on the host target
```

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

Only the first tab is given a sidebar. Go to a tab without one and a sidebar
opens there, from the sidebars already running.

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
- `fixture.rs` — the hardcoded rows and notifications.
- `main.rs` — the plugin: the two regions, the nav order over them, and the
  event handling.
