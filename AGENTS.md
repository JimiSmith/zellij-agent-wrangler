# Working in this repository

Agent Wrangler draws a sidebar that shows coding agents with installed hooks
in a terminal multiplexer, and says whose turn it is. A daemon holds the agent
state. A sidebar draws it. The two are separate programs.

Use `ARCHITECTURE.md` for component boundaries and data flow, `PROGRESS.md` for
implementation history and measured multiplexer behavior, and `FEATURES.md`
for feature status. Read the parts relevant to the change. This file holds the
working rules.

## The crates

| Crate | Role |
| --- | --- |
| `agent-wrangler-core` | Agent records, the registry, labels, commands, and the client message format. Shared by every client and the daemon. |
| `agent-wrangler-ui` | Rows, the tree, frame composition, styling and ANSI. Draws into a ratatui buffer. |
| `agent-wrangler-sidebar` | Application state, the reducer, effects, session reconciliation and options. |
| `agent-wrangler` | The hook client, daemon, installer and platform integration in one executable; Windows also has a windowless twin. |
| `zellij-agent-wrangler` | The zellij plugin binary targets wasm; its library also builds on the host for tests. |
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

- Keep platform-specific process and socket behavior in
  `crates/agent-wrangler/src/platform/`. The installer also uses `cfg` for file
  permissions and the Windows client name; tests use it for system-specific
  assertions. Do not spread platform-specific runtime behavior elsewhere.
- Build no path by hand and write no separator. Socket names go through
  `GenericNamespaced`, which is a unix socket on unix and a named pipe on
  Windows.
- Run external tools by name and let the system resolve them. Use the current
  executable's path when starting this program's daemon or installing hooks.
- When launching a process from Rust, pass arguments separately without a shell.
- Both spellings of the end of a stream take one arm. A unix peer's read
  returns zero after a shutdown. A Windows client's read fails after a
  `DisconnectNamedPipe`. Code that waits for zero alone waits for ever on
  Windows.
- Windows gives a console to any program whose parent has none. That is why the
  client ships twice, as `agent-wrangler.exe` and `agent-wranglerw.exe`.

Check the Windows build from Linux with:

```
cargo clippy -p agent-wrangler -p agent-wrangler-core \
    -p agent-wrangler-ui -p agent-wrangler-sidebar -p tmux-agent-wrangler \
    --target x86_64-pc-windows-msvc --all-targets --locked -- -D warnings
```

This covers the same native crates as the Windows CI job, aimed at the Windows
target. Install that Rust target first. Clippy needs no linker, so it catches
compile and lint failures but not runtime failures.

`--all-targets` is necessary, and a plain `cargo check` is not enough. A test
module whose every test is `#[cfg(unix)]` is empty on Windows, and an import at
the top of it is then unused. Clippy fails on that, and a build of the library
alone never looks at it. CI runs on pull requests, pushes to `main`, and tags;
the cross-target command catches the problem locally before CI.

## Rule three: multiplexer behavior belongs in adapters

Zellij and tmux run today. Others can follow.

- `agent-wrangler-core`, `agent-wrangler-ui` and `agent-wrangler-sidebar` do not
  interpret multiplexer topology. Core carries selected location variables as
  opaque values; shared state and drawing speak of tabs, panes, rows and sessions.
- A multiplexer crate adapts. It converts host reports into the portable
  vocabulary and executes the effects it gets back.
- The daemon holds agent state and nothing about where it is shown. It learns a
  location only as opaque values captured from the environment.
- A new multiplexer should put its topology and effects in an adapter. Core may
  need to capture another opaque location variable, but shared state and drawing
  should not gain multiplexer-specific decisions.

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

Run focused checks while working. For complete local verification before a push
on Linux, use the repository root. Install Python 3, Bash, Cargo, tmux and
Zellij. The host build also needs a C compiler, `pkg-config` and OpenSSL
development headers; on Ubuntu or Debian, install them with:

```
sudo apt-get install --yes build-essential pkg-config libssl-dev
```

Add the Rust tools and wasm target:

```
rustup component add clippy rustfmt
rustup target add wasm32-wasip1
```

The live CI job uses Zellij 0.45.1 and the Ubuntu 24.04 tmux package. Check
the versions with `zellij --version` and `tmux -V` if a live test behaves
differently locally.
Run the following commands; together they cover the Linux Rust checks, both
independent core feature configurations, and every Python test and live step
script:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy -p zellij-agent-wrangler --target wasm32-wasip1 \
    --all-targets --locked -- -D warnings
cargo test --workspace --locked --no-fail-fast -- --include-ignored
cargo clippy -p agent-wrangler-core --no-default-features \
    --all-targets --locked -- -D warnings
cargo test -p agent-wrangler-core --no-default-features \
    --locked --no-fail-fast -- --include-ignored
cargo clippy -p agent-wrangler-core --no-default-features --features json \
    --all-targets --locked -- -D warnings
cargo test -p agent-wrangler-core --no-default-features --features json \
    --locked --no-fail-fast -- --include-ignored
cargo build -p agent-wrangler -p tmux-agent-wrangler --locked
cargo build -p zellij-agent-wrangler --target wasm32-wasip1 --locked
python3 tests/run_all.py
```

`--all` is necessary on `fmt` because the root is a virtual manifest. The plain
form covers the default members and not every crate.

The second clippy run is not a duplicate. The plugin ships as wasm, and the host
lint says nothing about the released artifact.

The core checks use separate Cargo invocations because workspace feature
unification enables `native` and can hide a broken minimal or `json`-only build.
The Rust test command includes ignored tests and doctests. One ignored test
starts a real tmux server, so tmux must be installed even for the Rust suite.
Build the native binaries before `tests/run_all.py`: its Python integration tests
run before the step scripts and launch `target/debug/agent-wrangler` and
`target/debug/tmux-agent-wrangler`. The wasm build matches the live CI setup.
Run only one live harness at a time; its scripts share fixture files and a test
daemon. `tests/run_all.py` refuses missing tools, skipped tests and empty suites.

The Windows and macOS CI jobs build and test the native crates rather than the
wasm plugin. On macOS, run these checks for all five native crates:

```
cargo clippy -p agent-wrangler -p agent-wrangler-core \
    -p agent-wrangler-ui -p agent-wrangler-sidebar -p tmux-agent-wrangler \
    --all-targets --locked -- -D warnings
cargo test -p agent-wrangler -p agent-wrangler-core \
    -p agent-wrangler-ui -p agent-wrangler-sidebar -p tmux-agent-wrangler \
    --locked --no-fail-fast -- --include-ignored
```

On Windows, use these one-line commands in PowerShell. The native test command
appends the exception that Windows CI uses:

```
cargo clippy -p agent-wrangler -p agent-wrangler-core -p agent-wrangler-ui -p agent-wrangler-sidebar -p tmux-agent-wrangler --all-targets --locked -- -D warnings
cargo test -p agent-wrangler -p agent-wrangler-core -p agent-wrangler-ui -p agent-wrangler-sidebar -p tmux-agent-wrangler --locked --no-fail-fast -- --include-ignored --skip tmux_location::tests::sidebar_registration_cleans_only_its_pane_and_tolerates_removed_panes
```

Add a new native crate to both platform jobs' crate lists. Windows skips only
this tmux pane-registration integration test because psmux does not support the
pane-local option it needs. Linux and macOS run that test.
The Windows and macOS jobs provide the runtime checks that a Linux local run
cannot provide.

`./dev.sh` builds the plugin and opens a live zellij session with a sidebar in
every tab.

The end to end harness drives a real program in a real pty and asserts on the
cells that land on the screen. `zellij action dump-screen` returns nothing for a
plugin pane, so this is the only way to see what a sidebar drew.

For focused commands and more harness details, use `tests/README.md`.

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

1. Relevant tests pass; run the full Rust and live suites before a push.
2. `cargo fmt --all` leaves nothing to change.
3. Clippy is clean at `-D warnings`.

Commit messages carry the reasoning, not a file list. Say what changed, and what the changes achieve.

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
