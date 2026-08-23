"""Tests for the pty driver in drive.py.

Every test here drives a real child through a real pty. The bash cases need no
multiplexer, so they always run. If the zellij binary is absent, the zellij
cases skip themselves, and the tmux cases do the same without tmux. They do not
report a pass.

The step scripts name paths relative to the repository root. Run the tests from
the repository root:

    python3 -m unittest discover -s tests -v
"""

import json
import os
import subprocess
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import drive  # noqa: E402
from drive import Runner, StepFailure, run_script, unescape, which  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
SCRIPTS = os.path.join(HERE, "scripts")
HAVE_ZELLIJ = which("zellij") is not None
HAVE_TMUX = which("tmux") is not None


def read_dump(name, outdir):
    with open(os.path.join(outdir, name + ".txt"), encoding="utf-8") as handle:
        text = handle.read()
    with open(os.path.join(outdir, name + ".sgr.json"), encoding="utf-8") as handle:
        sgr = json.load(handle)
    return text, sgr


class TestUnescape(unittest.TestCase):
    def test_escapes(self):
        self.assertEqual(unescape("plain"), b"plain")
        self.assertEqual(unescape("a\\rb"), b"a\rb")
        self.assertEqual(unescape("a\\nb"), b"a\nb")
        self.assertEqual(unescape("\\e[A"), b"\x1b[A")
        self.assertEqual(unescape("a\\tb"), b"a\tb")

    def test_control_keys(self):
        self.assertEqual(unescape("<C-c>"), b"\x03")
        self.assertEqual(unescape("<C-q>"), b"\x11")
        self.assertEqual(unescape("<C-o>d"), b"\x0fd")

    def test_double_backslash_stays_literal(self):
        # A script sends `printf '\033[7m'` in this way. The backslash must
        # survive, and the next letter must not become an escape.
        self.assertEqual(unescape("\\\\033"), b"\\033")
        self.assertEqual(unescape("\\\\r"), b"\\r")


class TestSessionSafety(unittest.TestCase):
    def test_guard_rejects_a_foreign_name(self):
        for name in ("main", "wrangler-proto", "my-work", ""):
            with self.assertRaises(StepFailure):
                drive.guard_session_name(name)

    def test_guard_accepts_the_harness_prefix(self):
        self.assertEqual(
            drive.guard_session_name("wrangler-test-smoke"), "wrangler-test-smoke"
        )

    def test_only_prefixed_names_are_recorded_for_cleanup(self):
        runner = Runner(verbose=False)
        runner._note_sessions("zellij --session wrangler-test-alpha attach main")
        runner._note_sessions("zellij delete-session wrangler-proto")
        self.assertEqual(runner.sessions, ["wrangler-test-alpha"])

    def test_cleanup_never_names_a_foreign_session(self):
        runner = Runner(verbose=False)
        runner.sessions = ["someone-elses-session"]
        with self.assertRaises(StepFailure):
            runner.cleanup()

    def test_blanket_kills_are_refused(self):
        runner = Runner(verbose=False)
        for command in ("zellij kill-all-sessions -y", "zellij delete-all-sessions -y"):
            with self.assertRaises(StepFailure):
                runner._step_sh(command)


class TestTmuxSafety(unittest.TestCase):
    """A tmux command must name the server of the harness.

    A tmux server is one process, and `-L` chooses which one. Without the flag,
    a command reaches the server that holds the sessions of the developer.
    """

    def test_a_command_without_the_server_flag_is_refused(self):
        for command in (
            "tmux new-session -s wrangler-test-tree",
            "tmux kill-server",
            "/usr/local/bin/tmux kill-server",
            "printf x | tmux load-buffer -",
            "tmux -L other kill-server",
            "sh -c 'tmux list-panes'",
        ):
            with self.assertRaises(StepFailure, msg=command):
                drive.guard_tmux_command(command)

    def test_a_command_naming_the_harness_server_is_allowed(self):
        for command in (
            "tmux -L wrangler-test new-session -s wrangler-test-tree",
            "tmux -L wrangler-test kill-server",
            "tmux -L wrangler-test split-window -h -l 30 ./target/debug/x",
        ):
            self.assertEqual(drive.guard_tmux_command(command), command)

    def test_the_tmux_client_of_this_project_is_not_a_tmux_command(self):
        # The client is called `tmux-agent-wrangler`. Its path holds the letters
        # of `tmux` and it runs no tmux command, so it needs no server flag.
        for command in (
            "target/debug/tmux-agent-wrangler",
            "cargo build -p tmux-agent-wrangler",
            "$PWD/target/debug/tmux-agent-wrangler > out.txt",
        ):
            self.assertFalse(drive.runs_tmux(command), command)
            self.assertEqual(drive.guard_tmux_command(command), command)

    def test_a_step_refuses_an_unguarded_tmux_command(self):
        runner = Runner(verbose=False)
        with self.assertRaises(StepFailure):
            runner._step_sh("tmux kill-server")
        with self.assertRaises(StepFailure):
            runner.spawn("tmux new-session -s wrangler-test-tree")

    def test_cleanup_ends_no_server_that_this_run_did_not_start(self):
        runner = Runner(verbose=False)
        self.assertFalse(runner.started_tmux)
        runner._note_sessions("cargo build -p tmux-agent-wrangler")
        self.assertFalse(runner.started_tmux)
        runner._note_sessions("tmux -L wrangler-test new-session -s wrangler-test-x")
        self.assertTrue(runner.started_tmux)


class TestStepsAgainstBash(unittest.TestCase):
    """The driver end to end with no zellij involved at all."""

    def setUp(self):
        self.outdir = os.path.join(HERE, "out", "unittest")
        self.runner = Runner(outdir=self.outdir, verbose=False)
        self.addCleanup(self.runner.cleanup)

    def test_prompt_echo_and_reverse_video(self):
        self.runner.run(
            [
                "env: PS1=rig>",
                "spawn: bash --noprofile --norc -i",
                "wait: rig>",
                "keys: echo he$(echo ll)o\\r",
                "wait: hello",
                "keys: printf '\\\\033[7mBAR\\\\033[0m\\\\n'\\r",
                "wait: BAR",
                "dump: unit_bash",
            ]
        )
        text, sgr = read_dump("unit_bash", self.outdir)
        self.assertIn("hello", text)
        self.assertEqual(sgr["unhandled"], 0)
        reverse = [run for run in sgr["runs"] if run["sgr"].get("reverse")]
        self.assertEqual([run["text"] for run in reverse], ["BAR"])

    def test_waitgone(self):
        self.runner.run(
            [
                "env: PS1=rig>",
                "spawn: bash --noprofile --norc -i",
                "wait: rig>",
                "keys: echo MARKER-HERE\\r",
                "wait: MARKER-HERE",
                # `clear` repaints the screen, so the marker leaves it.
                "keys: clear\\r",
                "waitgone: MARKER-HERE @ 5",
            ]
        )

    def test_sh_failure_stops_the_run_and_sh_opt_does_not(self):
        with self.assertRaises(StepFailure):
            self.runner.run(["sh: exit 3"])
        self.runner.run(["sh?: exit 3"])

    def test_unknown_step_is_rejected(self):
        with self.assertRaises(StepFailure):
            self.runner.run(["frobnicate: something"])

    def test_wait_without_a_child_is_rejected(self):
        with self.assertRaises(StepFailure):
            self.runner.run(["wait: anything"])

    def test_timeout_reports_the_screen_and_exits_non_zero(self):
        script = os.path.join(self.outdir, "will_fail.steps")
        os.makedirs(self.outdir, exist_ok=True)
        with open(script, "w", encoding="utf-8") as handle:
            handle.write(
                "env: PS1=rig>\n"
                "spawn: bash --noprofile --norc -i\n"
                "wait: rig>\n"
                "keys: echo ON-SCREEN\\r\n"
                "wait: ON-SCREEN\n"
                "wait: NEVER-APPEARS @ 1\n"
            )
        result = subprocess.run(
            [sys.executable, os.path.join(HERE, "drive.py"), script, "--quiet"],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("FAILED", result.stderr)
        self.assertIn("NEVER-APPEARS", result.stderr)
        # The dump must carry the screen as it was, and the raw tail. A run
        # that fails leaves no other evidence.
        self.assertIn("--- screen", result.stderr)
        self.assertIn("ON-SCREEN", result.stderr)
        self.assertIn("--- last", result.stderr)


class TestScriptFiles(unittest.TestCase):
    def test_bash_smoke_script(self):
        with open(os.path.join(SCRIPTS, "bash_smoke.steps"), encoding="utf-8") as handle:
            steps = handle.read().splitlines()
        outdir = os.path.join(HERE, "out")
        self.assertEqual(run_script(steps, outdir=outdir, verbose=False), 0)
        text, sgr = read_dump("bash_smoke", outdir)
        self.assertIn("hello", text)
        self.assertTrue(any(run["sgr"].get("reverse") for run in sgr["runs"]))


@unittest.skipUnless(HAVE_ZELLIJ, "zellij is not installed")
class TestZellij(unittest.TestCase):
    def test_smoke_script_draws_the_status_bar_and_reaps_the_session(self):
        path = os.path.join(SCRIPTS, "zellij_smoke.steps")
        with open(path, encoding="utf-8") as handle:
            steps = handle.read().splitlines()
        outdir = os.path.join(HERE, "out")
        result = subprocess.run(
            [sys.executable, os.path.join(HERE, "drive.py"), path, "--quiet"],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        text, sgr = read_dump("zellij_smoke", outdir)
        self.assertIn("wrangler-test-smoke", text)
        self.assertIn("Ctrl", text)
        self.assertNotIn("First Run Setup Wizard", text)
        # Zellij paints in 256 colors. If the screen holds no attributes, the
        # emulator dropped the style. Zellij did not omit it.
        self.assertGreater(len(sgr["runs"]), 10)
        self.assertNotIn("wrangler-test-smoke", drive.live_sessions())

    def test_cleanup_reaps_a_session_that_outlives_the_child(self):
        outdir = os.path.join(HERE, "out")
        runner = Runner(outdir=outdir, verbose=False)
        self.addCleanup(runner.cleanup)
        runner.run(
            [
                "env: ZELLIJ_CONFIG_DIR=tests/zellij-config",
                "spawn: zellij --session wrangler-test-reap",
                "wait: Ctrl @ 20",
                # Ctrl-o enters session mode, and d detaches. The session
                # stays alive with no client. Cleanup exists for that state.
                "keys: <C-o>d",
                "sleep: 2",
            ]
        )
        self.assertIn("wrangler-test-reap", drive.live_sessions())
        runner.cleanup()
        self.assertNotIn("wrangler-test-reap", drive.live_sessions())


@unittest.skipUnless(HAVE_TMUX, "tmux is not installed")
class TestTmuxTree(unittest.TestCase):
    """The tmux sidebar against a real tmux server.

    The sidebar draws into a pane, and tmux composites that pane into one
    stream for the terminal. That stream is the only place its output exists,
    so the harness replays it and asserts on the cells that land.
    """

    @classmethod
    def setUpClass(cls):
        path = os.path.join(SCRIPTS, "tmux_tree.steps")
        result = subprocess.run(
            [sys.executable, os.path.join(HERE, "drive.py"), path, "--quiet"],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            raise AssertionError(result.stdout + result.stderr)
        cls.outdir = os.path.join(HERE, "out")

    def dump(self, name):
        return read_dump(name, self.outdir)

    def test_the_tree_matches_the_windows_and_the_panes(self):
        text, _ = self.dump("tmux_tree_three")
        self.assertIn("0: editor", text)
        self.assertIn("1: logs", text)
        self.assertIn("2: notes", text)
        # A window contributes its panes as children.
        self.assertIn("\u2514\u2500", text, "a branch under a window")

    def test_a_window_opened_later_reaches_the_tree(self):
        one, _ = self.dump("tmux_tree_one")
        three, _ = self.dump("tmux_tree_three")
        self.assertNotIn("2: notes", one)
        self.assertIn("2: notes", three)

    def test_a_row_draws_the_tmux_index_and_not_its_own_order(self):
        # Window 1 closed. Window 2 keeps the number that the user types to
        # reach it. A row numbered by its order would say `1` here.
        text, _ = self.dump("tmux_tree_gap")
        self.assertIn("0: editor", text)
        self.assertIn("2: notes", text)
        self.assertNotIn("1: logs", text)
        self.assertNotIn("1: notes", text)

    def test_a_split_reaches_the_tree_as_a_second_pane(self):
        gap, _ = self.dump("tmux_tree_gap")
        split, _ = self.dump("tmux_tree_split")
        # The branch changes from one child to two: the first becomes `\u251c\u2500`.
        self.assertNotIn("\u251c\u2500", gap)
        self.assertIn("\u251c\u2500", split)

    def test_the_gutter_marks_where_the_user_is(self):
        _, sgr = self.dump("tmux_tree_split")
        text, _ = self.dump("tmux_tree_split")
        rows = [row for row in text.splitlines() if "\u258c" in row]
        # One mark for the window that the user is in, and one for the pane.
        self.assertEqual(len(rows), 2, text)
        self.assertGreater(len(sgr["runs"]), 5, "the sidebar drew no attributes")

    def test_the_sidebar_draws_on_the_screen_that_keeps_no_history(self):
        # A whole-pane drawing has nothing to scroll back through. On the
        # normal screen a host keeps a history for it anyway, which is two
        # thousand lines per sidebar in tmux by default, and a user who scrolls
        # that pane finds blank lines.
        path = os.path.join(self.outdir, "tmux_tree_screen.txt")
        with open(path, encoding="utf-8") as handle:
            said = handle.read().strip()
        self.assertEqual(said, "SIDEBAR history=0 alt=1")

    def test_quitting_gives_the_pane_back_as_the_sidebar_found_it(self):
        # `q` stops the sidebar. Leaving the alternate screen puts back what the
        # pane held, so the frame goes and the earlier output returns.
        drawing, _ = self.dump("tmux_tree_split")
        after, _ = self.dump("tmux_tree_after_quit")
        self.assertNotIn("WRANGLER-PANE-BEFORE", drawing, "the sidebar hid it")
        self.assertIn("WRANGLER-PANE-BEFORE", after, "the pane came back")
        self.assertIn("WRANGLER-PANE-AFTER", after, "the sidebar exited")
        self.assertNotIn("0: editor", after, "the frame is gone")

    def test_a_narrower_pane_is_drawn_again_at_the_width_it_now_has(self):
        # Tmux reports a resize to no program, and a resize changes nothing in the
        # state that the sidebar holds. A sidebar that drew only on a change of
        # that state would keep the frame of the old width until something else
        # moved, and tmux would show it cut to the new width.
        wide, _ = self.dump("tmux_tree_wide")
        narrow, _ = self.dump("tmux_tree_narrow")
        self.assertIn("1: resize-me-a-long-name", wide)
        # The name is cut to fit twenty columns, and the ellipsis marks the cut.
        # A leftover frame carries no ellipsis here, because tmux cuts a stored
        # line without one.
        self.assertIn("1: resize-me-a-l…", narrow)
        self.assertNotIn("1: resize-me-a-long-name", narrow)
        # Every other row is composed again as well. A pane title cut for
        # thirty-four columns is longer than twenty columns hold.
        self.assertIn("bash in zellij-agent-…", wide)
        self.assertNotIn("bash in zellij-agent-…", narrow)
        self.assertIn("bash in…", narrow)

    def test_the_sidebar_reached_the_client_of_this_build(self):
        # The client is found by name on PATH. A developer with the released
        # client installed has an older one there, and the sidebar then draws
        # the version mismatch instead of a tree. The steps script sets PATH on
        # the command that creates the pane, because a pane made from a host
        # `sh:` step takes the environment of the host.
        text, _ = self.dump("tmux_tree_three")
        self.assertNotIn("OUT OF STEP", text)


if __name__ == "__main__":
    unittest.main()
