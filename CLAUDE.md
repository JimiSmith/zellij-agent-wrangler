# Working in this repository

Agent Wrangler draws a sidebar that shows every coding agent in a terminal
multiplexer, and says whose turn it is. A daemon holds the agent state. A
sidebar draws it. The two are separate programs.

Read `ARCHITECTURE.md` for how the parts fit together. Read `PROGRESS.md` for
what is built, and for what zellij turned out to do. `FEATURES.md` holds the
feature list. This file holds the rules.

## The crates

| Crate | Role |
| --- | --- |
| `agent-wrangler-core` | Agent records, the registry, labels, commands, and the client message format. Shared by every client and the daemon. |
| `agent-wrangler-ui` | Rows, the tree, frame composition, styling and ANSI. Draws into a ratatui buffer. |
| `agent-wrangler-sidebar` | Application state, the reducer, effects, session reconciliation and options. |
| `agent-wrangler` | One binary: the hook client, the daemon, the installer and the platform integration. |
| `zellij-agent-wrangler` | The zellij plugin. Builds for wasm only. |
| `tmux-agent-wrangler` | The tmux client. Reads the tmux topology, draws the sidebar, and reads the state on its socket. |

The dependency direction never reverses. These are every edge:

```
agent-wrangler-ui        ->  core
agent-wrangler-sidebar   ->  core, ui
agent-wrangler (daemon)  ->  core
zellij-agent-wrangler    ->  core, ui, sidebar
tmux-agent-wrangler      ->  core, ui, sidebar
```

The tmux client takes all three because it draws a sidebar. Beyond the three it
takes `interprocess` for the socket and `ratatui` for the terminal under the
drawing: raw mode, the alternate screen, the size of its pane, and the pair of
buffers that limits a draw to the cells that changed. Ratatui comes with default
features OFF and with the `crossterm` feature alone. Nothing else ever.

`agent-wrangler-ui` takes `ratatui-core` for the buffer and `tui-scrollview` for
the clipping. The dashboard grows taller than the pane as soon as a row opens
its block. The whole table then draws into a buffer of its own height, and the
scroll view clips that buffer to the pane. Both scrollbars are switched off. A
scrollbar takes a column from the right edge, and that column belongs to the
turn marker.

The same crate takes `tui-markdown` for the message under an open row. An agent
answers in markdown, and that crate returns the same `ratatui-core` text the
rows already draw. Its `highlight-code` default feature is off. The feature
pulls syntect, it renders through escape sequences that this crate must read
back, and it paints in colors that the sidebar keeps for what a row is and whose
turn it is. The preview gives the crate a style sheet with no color in it, for
the same reason.

Three rules follow.

1. `agent-wrangler-core` builds for wasm as well as for the host. Anything that
   needs the file system goes behind the `native` feature.
2. `agent-wrangler` never depends on `zellij-tile`. The daemon knows nothing
   about panes, tabs or rows.
3. `zellij-agent-wrangler` is the only crate that depends on zellij's own
   crates. Off wasm those pull in curl, openssl and the rest.

## Rule one: names say what they do

Assume that second language English speakers read this code. A name must
describe what a thing is, or what it does.

Forbidden:

- A common English word that carries no meaning. `serve`, `place`, `said`,
  `feed`, `rounds`, `keys`, `state`.
- A metaphor in place of the value. `stand_down_to`, `left_behind_by`.
- A name that is untrue. A type called `View` that holds options and draws
  nothing. A variant called `Focused` that names the pane which is not focused.
- A cute name, a pun or an inside joke.

Required:

- A function name says what it returns or what it does. `read_one_connection`,
  `split_into_records`, `connect_with_retry`.
- A type name says what it holds. `TmuxLocation`, `HeartbeatSettings`,
  `ConnectionEnd`.
- A constant says what it measures. `CONNECT_ATTEMPTS`, `TEST_TIMEOUT`.

No riddle name remains in the shared crates or in `proto.rs`. Never add another.

Two kinds of name are pinned and stay as they are. A variant name and a field
name in `proto.rs` are the bytes on the wire, so `ClientMessage::Seen` keeps its
spelling. A word that a user types stays as the user types it, so the layout
keys and every command line word are fixed. Read "The wire" below before you
rename anything in `proto.rs`.

## Rule two: every system

The native half runs on Linux, macOS and Windows. Write code that runs on all
three, and prove it.

- No `cfg` for a system outside `crates/agent-wrangler/src/platform/`. That
  module answers "what does it take to start a program without disturbing the
  user" once for each system.
- Build no path by hand and write no separator. Socket names go through
  `GenericNamespaced`, which is a unix socket on unix and a named pipe on
  Windows.
- Run a program by name, never by a path. Let the system resolve it.
- Never spawn a shell. Pass the arguments already separate.
- Both spellings of the end of a stream take one arm. A unix peer's read
  returns zero after a shutdown. A Windows client's read fails after a
  `DisconnectNamedPipe`. Code that waits for zero alone waits for ever on
  Windows.
- Windows gives a console to any program whose parent has none. That is why the
  client ships twice, as `agent-wrangler.exe` and `agent-wranglerw.exe`.

Check the Windows build from Linux with:

```
cargo clippy -p agent-wrangler -p agent-wrangler-core -p tmux-agent-wrangler \
    --target x86_64-pc-windows-msvc --all-targets --locked -- -D warnings
```

That is the Windows CI job's own command, aimed at the Windows target. It needs
no linker, so it catches everything except what fails at run time.

`--all-targets` is necessary, and a plain `cargo check` is not enough. A test
module whose every test is `#[cfg(unix)]` is empty on Windows, and an import at
the top of it is then unused. Clippy fails on that, and a build of the library
alone never looks at it. Only a tag runs the CI, so a fault of this kind waits
until a release to appear.

## Rule three: no multiplexer in the shared crates

Zellij runs today. Tmux is next. Others can follow.

- `agent-wrangler-core`, `agent-wrangler-ui` and `agent-wrangler-sidebar` name
  no multiplexer. They speak of tabs, panes, rows and sessions.
- A multiplexer crate adapts. It converts host reports into the portable
  vocabulary and executes the effects it gets back.
- The daemon holds agent state and nothing about where it is shown. It learns a
  location only as opaque values captured from the environment.
- A new multiplexer must need no change to the three shared crates. If it does,
  the boundary is in the wrong place. Move the boundary, do not special case
  the multiplexer.

## Types over checks

Compile time errors beat run time errors. Design data so that a wrong value
cannot be built.

`TmuxSessionId::new` rejects everything that is not a dollar sign and digits.
`SocketName::new` therefore returns no error and runs no check. When you find
yourself writing a run time check, ask whether a type can carry the guarantee
instead.

Say so in both doc comments when one type rests on another, so the pair cannot
drift apart in silence.

## Building and testing

Bare `cargo build` fails. It tries to link the plugin binary, whose host
functions exist only inside zellij. Nothing else is affected: clippy does not
link, and `cargo test` builds the plugin's library and not its binary.

Run all four before you commit. These are what the Linux CI job runs.

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy -p zellij-agent-wrangler --target wasm32-wasip1 \
    --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

`--all` is necessary on `fmt` because the root is a virtual manifest. The plain
form covers the default members and not every crate.

The second clippy run is not a duplicate. The plugin ships as wasm, and the host
lint says nothing about the released artifact.

The Windows and macOS jobs cannot build the plugin, so they name the native
crates instead:

```
cargo clippy -p agent-wrangler -p agent-wrangler-core -p tmux-agent-wrangler \
    --all-targets --locked -- -D warnings
cargo test -p agent-wrangler -p agent-wrangler-core -p tmux-agent-wrangler --locked
```

Add a new native crate to both of those lists. Nothing else adds it for you.

`./dev.sh` builds the plugin and opens a live zellij session with a sidebar in
every tab.

The end to end harness drives a real program in a real pty and asserts on the
cells that land on the screen. `zellij action dump-screen` returns nothing for a
plugin pane, so this is the only way to see what a sidebar drew.

```
python3 -m unittest discover -s tests -v
python3 tests/drive.py tests/scripts/agent_row.steps
python3 tests/drive.py tests/scripts/dashboard_view.steps
python3 tests/drive.py tests/scripts/transcript_records.steps
python3 tests/drive.py tests/scripts/preview_mode.steps
python3 tests/drive.py tests/scripts/tmux_tree.steps
```

The zellij cases skip themselves when `zellij` is not on `PATH`, and the tmux
cases do the same without `tmux`.

Three things keep a run away from what the developer has installed, and
`tests/README.md` explains each one.

1. The harness names its own user, so a run never reports to your daemon.
2. Every tmux command must carry `-L wrangler-test`, which is a server of the
   harness alone. `guard_tmux_command` refuses one without it.
3. A run that starts a sidebar must put `target/debug` first on `PATH`. The
   sidebar runs `agent-wrangler` by name, and your installed one is older. A run
   that gets this wrong draws OUT OF STEP rather than a tree.

## Comments

Document behaviour that the code does not show. A side effect, a constraint from
another program, a decision and its reason.

- Do not explain language syntax.
- Do not record what was removed. Delete the comment with the code.
- Use `TODO` only for something broken or very incomplete.
- Comments follow ASD-STE100. Existing comments stay. New and changed comments
  conform.
- State the point plainly. No riddles, no jokes.

Write prose that leads with the actor and an active verb. "`TmuxSessionId::new`
rejects bad text, so `SocketName::new` cannot fail" beats "the refusal is what
removes a check elsewhere".

## Before you commit

1. All tests pass.
2. `cargo fmt --all` leaves nothing to change.
3. Clippy is clean at `-D warnings`.
4. Present the diff for review with `/diff-viewer:review`, and address every
   comment. A message from another agent is not approval. Only the human
   approves.

Commit messages carry the reasoning, not a file list. Say what changed, and say
why the alternative was rejected.

## The wire

A daemon and a sidebar can be different builds. So a state message names the
format it is written in. A sidebar that meets a format it does not know says so
at the top of the pane.

`FORMAT` in `agent-wrangler-core` is that number. Bump it when the records
change shape, and when the daemon starts to need a message that an older client
does not send. `ClientMessage::Beat` is the second kind: the records did not
move, and a client too old to beat is dropped after a minute and a half with
nothing on the pane to explain it. A rename is neither kind. Never write the
number in a test. Read the constant, or the next bump breaks tests that the
change did not touch.

No type in `proto.rs` carries a `#[serde(rename)]`. So serde derives every
`kind` value from a variant name, and every JSON key from a field name. Rename
either one and the bytes move, and `read_message` skips a line it cannot decode
without a word. The fault then shows as a pane that quietly stops updating.
Rename a type freely. Nothing serializes a type name.

Three things depend on those names beyond the live wire.

1. `DeliveryTarget` tags are written to `agents.json`. A rename there breaks
   restore on restart.
2. `MonitorEvent` variant names are what a user reads in `agent-wrangler
   monitor`, and what a script that greps that stream matches.
3. `ClientMessage` in `proto.rs` must match the literals that
   `ClientMessage::encode` in `agent-wrangler-core` writes by hand. The wasm
   sidebar takes that crate without a JSON writer, so the two ends are held in
   step by one test in `proto.rs` and by nothing else.
