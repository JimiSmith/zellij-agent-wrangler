"""Tests for the pty driver in drive.py.

Every test here drives a real child through a real pty. The bash cases need no
zellij, so they always run. If the zellij binary is absent, the zellij cases
skip themselves. They do not report a pass.

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


if __name__ == "__main__":
    unittest.main()
