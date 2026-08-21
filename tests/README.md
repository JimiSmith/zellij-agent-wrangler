# End to end harness

Drives a real program in a real pty and asserts on what actually lands on the
screen. Python 3 standard library only, no third party packages.

## Why a pty and not a mock

Zellij renders to a terminal. The sidebar plugin does not return a string that a
test can inspect; it writes cells into a pane, and zellij composites that pane
with every other pane into one stream of escape sequences aimed at whatever
terminal the user is sitting in front of. That stream is the only place the
plugin's output exists. There is nothing upstream of it to mock that would still
exercise the interesting parts: the pane's width, the truncation, the redraw
after a resize, and above all which cells carry which attributes are all decided
by zellij, not by the plugin. So the harness allocates a pty, runs zellij inside
it as the child of a fork, and replays the bytes coming back through an ANSI
emulator that keeps attributes per cell. What the assertions see is what a
terminal would have drawn.

## Running

From the repository root:

    python3 -m unittest discover -s tests -v      # all of it
    python3 -m unittest tests.test_screen -v      # emulator only, no pty
    python3 tests/drive.py tests/scripts/bash_smoke.steps
    python3 tests/drive.py tests/scripts/zellij_smoke.steps

The zellij cases skip themselves when `zellij` is not on `PATH`.

Dumps are written to `tests/out/`, which is not tracked.

`drive.py` takes `--rows`, `--cols`, `--outdir` and `--quiet`, and reads the
step list from stdin when the script argument is `-`.

## Session safety

Any zellij session the harness starts is named with the `wrangler-test-` prefix.
Cleanup only ever deletes names carrying that prefix, and only names that a
command in the run itself mentioned; `guard_session_name` raises on anything
else. `zellij kill-all-sessions` and `zellij delete-all-sessions` are refused
outright as `sh:` steps. A developer's own sessions are never in reach of a test
run.

## Which daemon a run reaches

The daemon's socket is named for the user and for nothing else. A developer of
this project has the real client installed, and its daemon holds that name. A
run that does not say otherwise therefore reports to that daemon and asserts on
it. The build under test is never exercised at all. Nothing fails, so this
is silent until a change arrives that the installed daemon cannot carry.

Every script that starts a daemon must give the run a user of its own:

    env: USER=wrangler-test

An `env:` step reaches the children that the harness spawns, and not a `sh:`
step, which runs on the host. Any host script that runs the client must set the
same name itself. `expect-turn.sh` and `raise-call.sh` are the examples.

## Steps

A script is one step per line, `name: argument`. Blank lines and lines starting
with `#` are ignored.

| Step | What it does |
| --- | --- |
| `env: NAME=value` | Set a variable for children spawned after this line |
| `spawn: <command>` | Fork a child on a pty and run the command in it. Replaces any previous child |
| `sh: <command>` | Run a shell command on the host, outside the pty. Fails the run on a non-zero exit |
| `sh?: <command>` | The same, but a non-zero exit is allowed |
| `keys: <literal>` | Write bytes to the pty. `\r`, `\n`, `\e`, `\t`, `\\` and `<C-x>` are understood |
| `wait: <substring>` | Pump output until the substring appears on the replayed screen |
| `waitgone: <substring>` | Pump until it is no longer on the screen |
| `sleep: <seconds>` | Keep pumping for that long |
| `resize: <rows>x<cols>` | Change the window size and clear the grid, so the child redraws |
| `dump: <name>` | Write `tests/out/<name>.txt` and `tests/out/<name>.sgr.json` |

`wait:` and `waitgone:` default to a 10 second timeout. A trailing `@ <seconds>`
overrides it: `wait: Ctrl @ 20`.

On any failure the run prints the whole current screen with row numbers, the
count of escape sequences the emulator did not recognise, and the last 2KB of
raw bytes, then exits non-zero.

## Dump format

`<name>.txt` is the grid as text, one line per row, trailing blanks removed.

`<name>.sgr.json` carries the attributes. It lists `runs`: contiguous stretches
of one row that share a single set of attributes, with cells at default
attributes left out. So the whole reverse video question is one assertion:

    reverse = [r for r in dump["runs"] if r["sgr"].get("reverse")]
    assert len(reverse) == 1, "more than one pane drew a selection bar"

It also carries `unhandled` and `unhandled_seen`. A non-zero `unhandled` means
the program under test emitted something the emulator skipped, and the screen
being asserted on may no longer match reality. Treat it as a signal to extend
`screen.py`, not as noise.

## Assertion API (`screen.py`)

    screen.text()             -> list of rows as strings
    screen.line(n)            -> one row
    screen.cell(row, col)     -> Cell(char, sgr)
    screen.find("needle")     -> (row, col) or None
    screen.sgr_of("needle")   -> the Sgr on the first cell of the match, or None
    screen.runs()             -> [Run(row, col, text, sgr)] for everything styled
    screen.unhandled          -> count of sequences that were skipped

`Sgr` holds `fg`, `bg`, `bold`, `dim`, `reverse`, `underline` and `italic`.
`fg` and `bg` are the colour introducer as written, so plain red is `(31,)` and
palette index 9 is `(38, 5, 9)`; keeping the parameters rather than a resolved
colour means an assertion is about what the plugin emitted.

## Zellij configuration

`tests/zellij-config/config.kdl` is what runs point `ZELLIJ_CONFIG_DIR` at. With
no config file zellij opens its first run setup wizard, which covers the screen
and eats keystrokes; with the developer's own config a test result would depend
on their keybindings and theme. The file also turns off startup tips and release
notes, which otherwise open over the layout.

## Driving the plugin

Once the workspace builds, a script for the sidebar looks like the smoke test
with a layout added. The layout has to be generated first because `dev.kdl`
carries a `PLUGIN_LOCATION` placeholder for the wasm's absolute path:

    sh: cargo build --target wasm32-wasip1 -p zellij-agent-wrangler
    sh: sed "s#PLUGIN_LOCATION#file:$PWD/target/wasm32-wasip1/debug/zellij-agent-wrangler.wasm#" dev.kdl > tests/out/layout.kdl
    env: ZELLIJ_CONFIG_DIR=tests/zellij-config
    spawn: zellij --session wrangler-test-sidebar --new-session-with-layout tests/out/layout.kdl
    wait: Ctrl @ 20
    dump: sidebar

`--new-session-with-layout` rather than `--session` with `--layout`, because the
latter attaches to an existing session and silently ignores the layout.

## What this cannot do yet

- No alternate screen buffer. `CSI ?1049h` and `?1049l` are recognised and
  skipped, so a program that switches buffers keeps drawing on the one grid.
  Nothing that matters for a full screen application, but it means the screen
  after zellij exits still shows zellij.
- No scrollback. `CSI 3J` is a no-op and rows scrolled off the top are gone,
  so a test cannot assert on history.
- No wide character handling. A double width glyph occupies one cell, so column
  positions to the right of CJK text or some emoji will be off.
- Attribute tracking stops at the SGR parameters listed above. Blink, conceal
  and strikethrough are recognised but not stored per cell.
- `wait:` matches within a single row. A string that wraps across rows will
  never be found.
- One child at a time. A second `spawn:` replaces the first rather than running
  both, so a test cannot drive two clients attached to one session.
