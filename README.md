# zellij-agent-wrangler

A zellij sidebar that shows your tabs and panes as a tree, with the name and the
state of each coding agent in them.

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

- The sidebar shows every agent in the session, on the pane that runs it. The
  label is the name that the session gives itself.
- `○` shows that an agent is mid-turn. `●` shows that the agent wants you. The
  sidebar lists the calls for you at the foot of the pane.
- `Enter` or a click on a row moves the focus to that tab, pane or agent.
- An optional desktop notification tells you that an agent needs you. One call
  gives one notification, whatever the number of tabs.
- The sidebar supports Claude Code and GitHub Copilot CLI, on Linux, macOS and
  Windows.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/JimiSmith/zellij-agent-wrangler/main/install.sh | sh
```

```powershell
irm https://raw.githubusercontent.com/JimiSmith/zellij-agent-wrangler/main/install.ps1 | iex
```

The script installs the client, adds the client to the hooks of your agents, and
prints the block.

Put the block in your zellij layout, in **both** `default_tab_template` and
`new_tab_template`, beside `children`. A tab that you open later then also gets
a sidebar.

```kdl
pane size=32 borderless=true {
    plugin location="https://github.com/JimiSmith/zellij-agent-wrangler/releases/download/v0.1.13/zellij-agent-wrangler-v0.1.13.wasm" {
        install_hooks "/home/you/.local/bin/agent-wrangler"
    }
}
```

Start a new zellij session. The sidebar is there. **The first run is blank until
you answer the permission prompt of zellij.** See
[Troubleshooting](#troubleshooting).

You must install a [Nerd Font](https://www.nerdfonts.com/) for the pane icons
and the agent icons.

**Updates.** Zellij downloads the plugin once and holds it. To update, run this
script again. Then change the version in the url to match. Zellij tells one
build of the plugin from another by the url.

<details>
<summary>Windows notes</summary>

The script installs the client in `%LOCALAPPDATA%\Programs\agent-wrangler`. The
script does not add the client to your `PATH`.

To change that, pass `-AddToPath` or `-Bin <dir>`. A script piped into `iex`
takes no arguments. Run the command below instead:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/JimiSmith/zellij-agent-wrangler/main/install.ps1))) -AddToPath
```

The script installs two copies of the client. `agent-wrangler.exe` is the copy
to run yourself. `agent-wranglerw.exe` is the same build, linked so that Windows
never gives it a console.

When the parent process has no console, Windows draws a console window for a
console program. An agent that runs a hook has no console. A zellij server that
runs the client for the sidebar has no console. The hooks and the block
therefore name `agent-wranglerw.exe`, and no window appears. Any command that
you start by hand needs `agent-wrangler.exe`.

The path in the block is a KDL string. In a KDL string a backslash starts an
escape. The path therefore has doubled separators. The script prints the path in
that form. A raw path gives you a layout that zellij cannot load.

```kdl
install_hooks "C:\\Users\\you\\AppData\\Local\\Programs\\agent-wrangler\\agent-wranglerw.exe"
```

A zellij under WSL counts as a different machine. Install the client there too,
with `install.sh`. Make the layout there name that client.

</details>

## Keys

Zellij starts a session in locked mode. Locked mode sends keystrokes to the
focused pane, and the sidebar works in locked mode.

To move the focus onto the sidebar, press `Ctrl+g` to unlock. Then press
`Ctrl+p` and `←`.

| Key                   | Does                                             |
| --------------------- | ------------------------------------------------ |
| `j` / `k` / `↑` / `↓` | Move the selection                               |
| `Enter` / click       | Go to the selected tab, pane or agent            |
| `q`                   | Turn the sidebar off for the rest of the session |

The row of a tab takes you to the first pane in that tab, not to the tab itself.
The last focus in a tab is often the sidebar of that tab.

The sidebars of a session share one selection. Only the sidebar that your keys
reach draws the selection bar. The `▌` gutter shows your position, and every
sidebar shows that gutter.

## Agents

When the lifecycle hooks of an agent call the client, the agent appears. The
install script sets up these hooks.

To set up the hooks by hand, or after you move the client, run one of these
commands:

```bash
agent-wrangler install-hooks             # or: claude, copilot
agent-wrangler install-hooks --uninstall
```

The client merges the hooks of Claude into `~/.claude/settings.json`. The client
keeps everything else in the file, and writes a `.agent-wrangler.bak` copy. The
client replaces or removes only the commands that run the client.

The hooks fire for every agent that you start, anywhere. The client does nothing
outside zellij.

The label of an agent is the title that the session gives itself. If the session
has no title, the label is the working directory of the session. The label of a
teammate is `@name - title`. A pane with two agents gets one row for each agent.

The sidebar draws the icon in the color that the agent shows for that session
(the `/color` command of Claude). The color comes from the palette of your
terminal, and it follows your theme. Color tells you *which* agent it is. Color
never tells you which row is live.

## Options

The options go in the block of the plugin. This example shows every option at
its default value. The same block goes in both templates.

```kdl
plugin location="..." {
    label "name"                 // agent rows: 'name' (session title, or the
                                 // working directory) | 'dir'
    sections false               // one section for each agent, below the tree
    turn_state true              // '○' mid-turn, '●' when the agent wants you
    notifications true           // the calls for you, at the foot
    desktop_notification "off"   // 'off' | 'on' (notify-send) | a command line
    install_hooks "off"          // 'off' | 'on' | a path to the client
}
```

If an option does not recognize a value, the option keeps its default value. A
typo costs you the setting, not the sidebar.

`install_hooks` also tells the sidebar where the client is. The sidebar reaches
the agent state only through the client.

A path is the safest value. The value `on` means "on `$PATH`". The `$PATH` that
matters is the `$PATH` of the zellij server. The zellij server inherits that
`$PATH` from the process that started zellij, not from the shell that you
installed from. The install script and `dev.sh` write the path in for you.

`sections on` draws the tree. Below the tree it draws the same sessions again,
in a group for each agent, not in a group for each tab.

### Desktop notifications

`desktop_notification "on"` uses `notify-send`. Any other value is a command
line. The command line runs with the name of the agent and the name of the
session as the last two arguments.

One call gives one notification, whatever the number of tabs and sessions that
hold the call. At most one notification goes out every five seconds, because
agents call in flurries.

The command runs with `ZELLIJ_SESSION_NAME`, `ZELLIJ_PANE_ID`, `TMUX`,
`TMUX_PANE` and `ZELLIJ` set to the values that the agent reported. A notifier
can therefore speak back to the session that made the call:

```sh
#!/bin/sh
set -eu
zellij --session "${ZELLIJ_SESSION_NAME:?}" pipe "$(printf 'zjstatus::notify::%s - %s' "${1:-}" "${2:-}")"
```

Name the session in your command, as the example does. Do not let the notifier
inherit the session name. The notifier inherits the environment of the pane that
started the daemon. That session can be long gone.

## Troubleshooting

**The sidebar is blank on the first run, and again after some upgrades.** Zellij
asks for permissions, and the question is not on screen yet. Focus the sidebar
with `Ctrl+g`, then `Ctrl+p` and `←`. Press `y`. Zellij caches the answer in
`~/.cache/zellij/permissions.kdl`, one answer for each plugin url. You answer
once for each machine, but every tab that already existed asks for itself. The
sidebar needs `ReadApplicationState`, `ChangeApplicationState`, `RunCommands`
and `MessageAndLaunchOtherPlugins`. See
[zellij#4749](https://github.com/zellij-org/zellij/issues/4749).

**The tree is there, no agents are there, and a message is at the top of the
pane.** The sidebar failed to run the client. The message gives the output of
the run. Make sure that the path in `install_hooks` is correct.

**A message about versions is at the top of the pane.** The plugin and the
client come from different releases. Run the install script again. Then put the
version that the script prints into the url in your layout.

**The first click on a row does nothing.** Zellij uses that click to focus the
pane. Set `mouse_click_through true` in your own zellij config. The plugin does
not set it for you, because the setting applies to every pane in every session.

**Boxes appear instead of icons.** The terminal needs a Nerd Font.

The daemon holds the agent state. Two commands show what the daemon holds:

```bash
agent-wrangler agents      # what it holds, in the form that it sends
agent-wrangler monitor     # every message in and out, as it happens
```

## Development

```bash
rustup target add wasm32-wasip1
./dev.sh       # builds both halves and opens a session with the sidebar in every tab
cargo test     # everything that does not call zellij, on the host target
```

`ARCHITECTURE.md` describes the structure. `FEATURES.md` lists what the project
does and what is left. `PROGRESS.md` records what the design rests on.
