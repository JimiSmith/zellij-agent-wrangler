# Pane-local sidebar identification

Each sidebar sets `@agent-wrangler-sidebar=1` on its own pane before it
starts terminal setup or topology refreshes. Registration and cleanup use
`tmux -S <server_socket> set-option -p[u] -t <pane_id>` with the server and
pane captured from `TMUX` and `TMUX_PANE`. Ordinary scope exit attempts to
unset only that pane's flag. A missing pane or server makes cleanup fail
without replacing the sidebar's return value or printing over its terminal.
A registration failure is reported rather than ignored.

The option is reserved for pane-local use. Window, session and global values
are not supported. This is a display classification, not an ownership or
process-liveness record. A crash that leaves a surviving pane marked is an
accepted limitation. No recovery, process inspection or additional timer runs.

## Interface for presentation and focus adapters

`topology::ReportedPane::is_sidebar` is true only for the exact value `1`.
`topology::read_panes` retains every physical pane, including marked panes.
The pane report format is now:

    window_id TAB pane_id TAB pane_active TAB sidebar_flag TAB title

The free-text title remains last, so embedded tabs survive. Both the polling
command and control-mode command use `topology::PANE_FORMAT`, and both replies
reach the same parser. The existing refresh mechanism observes flag changes.

The adapter excludes marked panes from content layouts while preserving the
raw physical reports for peer focus and automatic-close decisions. A neutral
excluded-pane input also filters registry-derived notifications and invalidates
cached selections and click targets. Authoritative agent records remain intact;
an agent returns to the view when its surviving pane's flag is removed.

## Verification

Unit tests cover exact flag classification, titles containing tabs, control
reply framing, polling replies, and registration/cleanup command targets on
Unix socket and Windows named-pipe spellings.

Run the isolated real-tmux lifecycle test explicitly:

    cargo test -p tmux-agent-wrangler sidebar_registration_cleans --locked -- --ignored

It verifies registration, successful cleanup on a surviving pane, unchanged
peer flags, cleanup after the registered pane has already been removed, and
registration failure for a removed pane. It owns a PID-qualified test server.

Run the attached PTY startup/quit test:

    python3 tests/drive.py tests/scripts/tmux_sidebar_flag.steps

It verifies the running binary marks only its pane and unsets the option after
`q` returns to a surviving shell. The harness owns `-L wrangler-test`; do not
run its scripts concurrently.

Run the integrated attached-PTY matrix after building the native binaries:

    cargo build -p tmux-agent-wrangler -p agent-wrangler --locked
    python3 -m unittest discover -s tests -p test_tmux_sidebar_exclusion.py -v

The matrix checks all nine ordered view pairs, three simultaneous mixed views,
ordinary pane and agent controls, control and polling reclassification, peer
focus in one window and across windows, shell-child cleanup, root-process quit,
and physical sidebar-only companionship. It uses its own daemon user and a
temporary `TMUX_TMPDIR`, retaining `-L wrangler-test`. The socket directory also
isolates the daemon's session sink, whose name does not include the user.
Text, raw terminal streams and SGR evidence are saved under
`tests/out/sidebar-exclusion/`. Unit tests cover cached activation, preview and
notification exclusions, confirmed active-window state and absent daemon effects.

## Host compatibility

Linux with tmux 3.5a passed the live checks. Workspace, WASM and Windows-target
Clippy passed, but Windows/psmux and macOS were not runtime-tested. Compilation
does not prove pane-option behavior on either host.

The psmux argument reference at commit
`493d721deb28ab051ee8ad235ba5bbadd9bb1e88` documents pane scope for `set-option`
and `show-options` (#580), and pane value-only read fixes (#647):
https://github.com/psmux/psmux/blob/493d721deb28ab051ee8ad235ba5bbadd9bb1e88/docs/tmux_args_reference.md

This is documentation evidence, not validation of a released Windows binary.
A supported host that lacks correct pane-local set/unset, explicit server
selection or per-pane format lookup needs a separate compatibility decision.
Do not substitute inherited options, title/process heuristics or a new
ownership protocol, and do not treat a green cross-build as permission to
remove existing platform support.
