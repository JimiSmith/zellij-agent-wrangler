# zellij-agent-wrangler

A zellij sidebar listing your tabs and panes as a tree, with the coding agents
running in them: what each one is called, which one is working, and which one is
waiting for you.

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

- Every agent in the session, on the pane it is running in, labelled with what
  the session calls itself.
- `○` says an agent is mid-turn, `●` says it wants you, and the calls for you
  are listed at the foot of the pane.
- `Enter` or a click on any row goes there.
- Optional desktop notification when an agent needs you, once per call however
  many tabs are watching.
- Claude Code and GitHub Copilot CLI, on Linux, macOS and Windows.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/JimiSmith/zellij-agent-wrangler/main/install.sh | sh
```

```powershell
irm https://raw.githubusercontent.com/JimiSmith/zellij-agent-wrangler/main/install.ps1 | iex
```

The script installs the client, wires it into your agents' hooks, and prints a
layout block. Put that block in your zellij layout inside **both**
`default_tab_template` and `new_tab_template`, beside `children`, so tabs opened
later get a sidebar too:

```kdl
pane size=32 borderless=true {
    plugin location="https://github.com/JimiSmith/zellij-agent-wrangler/releases/download/v0.1.10/zellij-agent-wrangler-v0.1.10.wasm" {
        install_hooks "/home/you/.local/bin/agent-wrangler"
    }
}
```

Start a new zellij session and the sidebar is there. **The first run comes up
blank until you grant zellij's permission prompt** — see
[Troubleshooting](#troubleshooting).

You need a [Nerd Font](https://www.nerdfonts.com/) for the pane and agent icons.

**Updating.** Run the script again and change the version in the url to match.
Zellij caches a plugin under the last part of its url and never fetches that
name twice, so the version has to change for the new one to be loaded.

<details>
<summary>Windows notes</summary>

The client goes in `%LOCALAPPDATA%\Programs\agent-wrangler` and is not put on
your `PATH`. Pass `-AddToPath` or `-Bin <dir>` to change that — a script piped
into `iex` takes no arguments, so run it as a block instead:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/JimiSmith/zellij-agent-wrangler/main/install.ps1))) -AddToPath
```

Two copies of the client are installed. `agent-wrangler.exe` is the one to run
yourself, and `agent-wranglerw.exe` is the same build linked so that Windows
never gives it a console. Windows draws a console window for a console program
whose parent has none, which is what an agent running a hook and a zellij server
running the sidebar's client both are, so the hooks and the layout name the
second one and nothing flashes up. Anything you run by hand wants the first.

The path in the layout block is a KDL string, where a backslash starts an
escape, so it is written with the separators doubled. The script prints it that
way; pasting a raw path gives you a layout that will not load.

```kdl
install_hooks "C:\\Users\\you\\AppData\\Local\\Programs\\agent-wrangler\\agent-wranglerw.exe"
```

A zellij running under WSL is a different machine as far as this is concerned.
Install there too, with `install.sh`, and let that layout name that client.

</details>

## Keys

Zellij starts a session in locked mode, which passes keystrokes to the focused
pane, so the sidebar works while locked. `Ctrl+g` unlocks, which is what you
need to move focus onto the sidebar (`Ctrl+p`, then `←`).

| Key                   | Does                                             |
| --------------------- | ------------------------------------------------ |
| `j` / `k` / `↑` / `↓` | Move the selection                               |
| `Enter` / click       | Go to the selected tab, pane or agent            |
| `q`                   | Turn the sidebar off for the rest of the session |

A tab's row takes you to the first pane in that tab rather than to the tab
itself, since where a tab was last left is as often as not its own sidebar.

The sidebars of a session share one selection, and only the one your keys reach
draws the selection bar. Where you are is the `▌` gutter, which every sidebar
shows.

## Agents

An agent appears once its own lifecycle hooks call the client, which the install
script sets up. To do it by hand, or after moving the binary:

```bash
agent-wrangler install-hooks             # or: claude, copilot
agent-wrangler install-hooks --uninstall
```

Claude's hooks are merged into `~/.claude/settings.json`, keeping everything
else in the file and leaving a `.agent-wrangler.bak` copy; only commands running
this client are ever replaced or removed. The hooks fire for every agent you
start anywhere, and the client does nothing outside zellij.

An agent is labelled with whatever the session has titled itself, falling back
to the directory it is working in. A teammate is labelled `@name - title`. A
pane running two agents gets a row each.

The icon is drawn in the colour the agent shows for that session (Claude's
`/color`), from your terminal's own palette, so it follows your theme. Colour
says *which* agent, never which row is live.

## Options

Options go in the plugin's block, and every one is shown here at its default.
Put the same block in both templates.

```kdl
plugin location="..." {
    label "name"                 // agent rows: 'name' (session title, falling
                                 // back to the directory) | 'dir'
    sections false               // a block per agent below the tree
    turn_state true              // '○' mid-turn and '●' when it wants you
    notifications true           // the calls for you, at the foot
    desktop_notification "off"   // 'off' | 'on' (notify-send) | a command line
    install_hooks "off"          // 'off' | 'on' | a path to the hook client
}
```

A value an option does not recognise leaves that option at its default, so a
typo costs you the setting rather than the sidebar.

`install_hooks` also tells the sidebar where the client is, which is how it
reaches the agent state at all. Give it a path: `on` means "on `$PATH`", and the
`$PATH` that matters is the zellij server's, inherited from whatever started
zellij rather than from the shell you installed from. The install script and
`dev.sh` write the path in for you.

`sections on` draws the tree, then the same sessions again grouped under the
agent running them rather than under their tab.

### Desktop notifications

`desktop_notification "on"` uses `notify-send`; anything else is run as a
command line, with the agent's name and the session's name as its last two
arguments. One call is one notification however many tabs and sessions are
holding it, and at most one every five seconds, since agents call in flurries.

The command is run with `ZELLIJ_SESSION_NAME`, `ZELLIJ_PANE_ID`, `TMUX`,
`TMUX_PANE` and `ZELLIJ` set to what the calling agent reported, so a notifier
can speak back to the session the call came from:

```sh
#!/bin/sh
set -eu
zellij --session "${ZELLIJ_SESSION_NAME:?}" pipe "$(printf 'zjstatus::notify::%s - %s' "${1:-}" "${2:-}")"
```

Name the session rather than letting it be inherited, as the example does: the
environment the notifier inherits is that of whichever pane happened to start
the background daemon, however long ago that session ended.

## Troubleshooting

**The sidebar is blank on the first run, and again after some upgrades.** Zellij
is asking for permissions and nothing has painted the question yet. Focus the
sidebar (`Ctrl+g`, then `Ctrl+p` and `←`) and press `y`. The answer is cached in
`~/.cache/zellij/permissions.kdl` per plugin url, so it is once per machine, but
every tab that already existed asks separately. The sidebar needs
`ReadApplicationState`, `ChangeApplicationState`, `RunCommands` and
`MessageAndLaunchOtherPlugins`. See
[zellij#4749](https://github.com/zellij-org/zellij/issues/4749).

**The tree is there but no agents are, and there is a message at the top of the
pane.** The sidebar could not run the client, and the message says what the run
said. Check the path in `install_hooks`.

**A message about versions at the top of the pane.** The plugin and the client
are from different releases. Run the install script again and put the version it
prints into the url in your layout.

**The first click on a row does nothing.** Zellij spends it on focusing the
pane. Set `mouse_click_through true` in your own zellij config; the plugin will
not set it for you, since it applies to every pane in every session.

**Boxes instead of icons.** The terminal needs a Nerd Font.

Two commands say what the background daemon holding the agent state knows:

```bash
agent-wrangler agents      # what it holds, as it would send it
agent-wrangler monitor     # every message in and out, as it happens
```

## Development

```bash
rustup target add wasm32-wasip1
./dev.sh       # builds both halves and opens a session with the sidebar in every tab
cargo test     # everything that does not call zellij, on the host target
```

`ARCHITECTURE.md` is how it is put together, `FEATURES.md` is what it does and
what is left, and `PROGRESS.md` is what the design rests on.
