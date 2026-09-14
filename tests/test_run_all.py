"""Full-suite policy tests use temporary suites and never start a multiplexer."""

import importlib.util
import io
import json
import os
import subprocess
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch


class HarnessFixture(unittest.TestCase):
    def setUp(self):
        runner_path = Path(__file__).with_name("run_all.py")
        self.runner_path = runner_path
        self.assertTrue(runner_path.is_file(), "the full-suite runner must exist")
        spec = importlib.util.spec_from_file_location("wrangler_run_all", runner_path)
        assert spec is not None and spec.loader is not None
        self.runner_module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.runner_module)
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.tests = self.root / "tests"
        self.tests.mkdir()
        self.output = io.StringIO()
        original_path = sys.path[:]
        self.addCleanup(lambda: sys.path.__setitem__(slice(None), original_path))

    def write_suite(self, body):
        module_name = "test_fixture_" + self.root.name
        (self.tests / (module_name + ".py")).write_text(
            "import unittest\n" + body, encoding="utf-8"
        )
        self.addCleanup(sys.modules.pop, module_name, None)


class TestStrictUnittests(HarnessFixture):
    def test_a_skipped_test_fails_the_full_suite_policy(self):
        self.write_suite(
            "class Fixture(unittest.TestCase):\n"
            "    @unittest.skip('fixture skip')\n"
            "    def test_skipped(self): pass\n"
        )
        report = self.runner_module.run_unittests(self.tests, self.output)
        self.assertFalse(report["success"])
        self.assertEqual(report["skipped"], 1)
        self.assertEqual(report["tests_run"], 1)
        self.assertIn("fixture skip", self.output.getvalue())

    def test_an_expected_failure_fails_the_full_suite_policy(self):
        self.write_suite(
            "class Fixture(unittest.TestCase):\n"
            "    @unittest.expectedFailure\n"
            "    def test_expected_failure(self): self.fail('known bug')\n"
        )
        report = self.runner_module.run_unittests(self.tests, self.output)
        self.assertFalse(report["success"])
        self.assertEqual(report["expected_failures"], 1)

    def test_a_test_import_error_is_reported_as_a_failure(self):
        self.write_suite("raise RuntimeError('broken import')\n")
        report = self.runner_module.run_unittests(self.tests, self.output)
        self.assertFalse(report["success"])
        self.assertIn("errors", report)
        self.assertEqual(report["errors"], 1)
        self.assertIn("broken import", self.output.getvalue())

    def test_a_suite_that_stops_early_cannot_report_success(self):
        self.write_suite(
            "class Fixture(unittest.TestCase):\n"
            "    def test_a_stop(self): self._outcome.result.stop()\n"
            "    def test_z_not_run(self): pass\n"
        )
        report = self.runner_module.run_unittests(self.tests, self.output)
        self.assertFalse(report["success"])
        self.assertEqual(report["discovered"], 2)
        self.assertEqual(report["tests_run"], 1)

    def test_an_unexpected_success_is_reported_as_a_failure(self):
        self.write_suite(
            "class Fixture(unittest.TestCase):\n"
            "    @unittest.expectedFailure\n"
            "    def test_unexpected_success(self): pass\n"
        )
        report = self.runner_module.run_unittests(self.tests, self.output)
        self.assertFalse(report["success"])
        self.assertIn("unexpected_successes", report)
        self.assertEqual(report["unexpected_successes"], 1)

    def test_discovery_includes_every_matching_python_file(self):
        for prefix in ("test_first_", "testsecond_", "test_third_"):
            module_name = prefix + self.root.name
            (self.tests / (module_name + ".py")).write_text(
                "import unittest\n"
                "class Fixture(unittest.TestCase):\n"
                "    def test_pass(self): pass\n", encoding="utf-8",
            )
            self.addCleanup(sys.modules.pop, module_name, None)
        (self.tests / "not_a_test.py").write_text("raise RuntimeError('not a test')")
        report = self.runner_module.run_unittests(self.tests, self.output)
        self.assertTrue(report["success"], self.output.getvalue())
        self.assertEqual(report["tests_run"], 3)
        self.assertEqual(report["discovered"], 3)

    def test_empty_discovery_fails_the_full_suite_policy(self):
        report = self.runner_module.run_unittests(self.tests, self.output)
        self.assertFalse(report["success"])
        self.assertEqual(report["tests_run"], 0)
        self.assertEqual(report["discovered"], 0)


class TestFullHarness(HarnessFixture):
    def test_local_and_ci_runs_clear_parent_multiplexer_identity(self):
        self.write_suite(
            "import os\n"
            "class Fixture(unittest.TestCase):\n"
            "    def test_environment(self):\n"
            "        for name in ('TMUX', 'TMUX_PANE', 'ZELLIJ', 'ZELLIJ_SESSION_NAME', 'ZELLIJ_PANE_ID'):\n"
            "            self.assertNotIn(name, os.environ)\n"
            "        self.assertTrue(os.path.isdir(os.environ['TMUX_TMPDIR']))\n"
            "        self.assertNotEqual(os.environ['TMUX_TMPDIR'], '/parent/socket')\n"
            "        self.assertEqual(os.environ['TERM'], 'xterm-256color')\n"
        )
        self.write_scripts(["environment.steps"])
        driver = self.tests / "drive.py"
        driver.write_text(
            "import os\n"
            "for name in ('TMUX', 'TMUX_PANE', 'ZELLIJ', 'ZELLIJ_SESSION_NAME', 'ZELLIJ_PANE_ID'):\n"
            "    assert name not in os.environ, name\n"
            "assert os.path.isdir(os.environ['TMUX_TMPDIR'])\n"
            "assert os.environ['TMUX_TMPDIR'] != '/parent/socket'\n"
            "assert os.environ['TERM'] == 'xterm-256color'\n"
        )
        parent_environment = {
            "TMUX": "/parent/server,1,0", "TMUX_PANE": "%1",
            "ZELLIJ": "0", "ZELLIJ_SESSION_NAME": "personal", "ZELLIJ_PANE_ID": "1",
            "TMUX_TMPDIR": "/parent/socket", "TERM": "dumb",
        }
        with patch.dict(os.environ, parent_environment):
            self.assertEqual(self.run_all(), 0, self.read_summary())
            for name, value in parent_environment.items():
                self.assertEqual(os.environ[name], value)

    def write_scripts(self, names):
        for name in names:
            script = self.tests / "scripts" / name
            script.parent.mkdir(parents=True, exist_ok=True)
            script.write_text("0", encoding="utf-8")
        (self.tests / "drive.py").write_text(
            "import json, os, pathlib, sys\n"
            "script = pathlib.Path(sys.argv[1])\n"
            "print(json.dumps({'script': str(script), 'cwd': os.getcwd(), "
            "'path': os.environ['PATH'], 'pwd': os.environ['PWD'], "
            "'shell': os.environ['SHELL'], 'home': os.environ.get('HOME')}))\n"
            "print('driver stderr', file=sys.stderr)\n"
            "sys.exit(int(script.read_text()))\n", encoding="utf-8",
        )

    def run_all(self, **kwargs):
        return self.runner_module.main(
            root=self.root, output=self.output,
            executable_lookup=lambda name, path: str(self.root / "bin" / name),
            **kwargs,
        )

    def read_summary(self):
        return json.loads((self.tests / "out/run-all/summary.json").read_text())

    def test_all_discovered_scripts_run_with_repository_environment(self):
        self.write_suite(
            "import os\n"
            "class Fixture(unittest.TestCase):\n"
            "    def test_environment(self):\n"
            f"        self.assertEqual(os.getcwd(), {str(self.root)!r})\n"
            f"        self.assertEqual(os.environ['PWD'], {str(self.root)!r})\n"
            f"        self.assertEqual(os.environ['SHELL'], {str(self.root / 'bin/bash')!r})\n"
        )
        names = ["z.steps", "nested/new.steps", "a.steps", "other/new.steps"]
        self.write_scripts(names)
        (self.tests / "scripts/ignored.txt").write_text("not a step script")
        original_environment = dict(os.environ)
        original_directory = Path.cwd()
        self.assertEqual(self.run_all(), 0, self.output.getvalue())
        report = self.read_summary()
        self.assertTrue(report["success"])
        self.assertTrue(report["unittest"]["success"])
        self.assertEqual(report["unittest"]["tests_run"], 1)
        self.assertEqual(
            [item["script"] for item in report["scripts"]],
            ["tests/scripts/" + name for name in sorted(names)],
        )
        for item in report["scripts"]:
            self.assertEqual(item["returncode"], 0)
            log = (self.root / item["log"]).read_text()
            self.assertIn("driver stderr", log)
            details = next(
                json.loads(line) for line in log.splitlines() if line.startswith("{")
            )
            self.assertEqual(details["cwd"], str(self.root))
            self.assertEqual(details["pwd"], str(self.root))
            self.assertEqual(details["shell"], str(self.root / "bin/bash"))
            self.assertEqual(details["home"], original_environment.get("HOME"))
            self.assertEqual(details["path"].split(os.pathsep)[0], str(self.root / "target/debug"))
        self.assertEqual(dict(os.environ), original_environment)
        self.assertEqual(Path.cwd(), original_directory)
        self.assertIn("test_environment", (self.root / report["unittest"]["log"]).read_text())

    def test_failures_do_not_prevent_later_scripts_from_running(self):
        self.write_suite(
            "class Fixture(unittest.TestCase):\n"
            "    def test_failure(self): self.fail('unit failure')\n"
            "    def test_later(self): pass\n"
        )
        self.write_scripts(["a.steps", "b.steps", "nested/c.steps"])
        (self.tests / "scripts/a.steps").write_text("7")
        self.assertEqual(self.run_all(), 1)
        report = self.read_summary()
        self.assertFalse(report["success"])
        self.assertEqual([item["returncode"] for item in report["scripts"]], [7, 0, 0])
        self.assertEqual(report["unittest"]["tests_run"], 2)
        self.assertIn("failures", report["unittest"])
        self.assertEqual(report["unittest"]["failures"], 1)
        self.assertIn("unit failure", (self.tests / "out/run-all/unittest.log").read_text())

    def test_a_script_launch_error_does_not_stop_the_remaining_scripts(self):
        self.write_suite("class Fixture(unittest.TestCase):\n    def test_pass(self): pass\n")
        self.write_scripts(["a.steps", "b.steps"])
        commands = []

        def run_command(command, **kwargs):
            commands.append(command)
            if len(commands) == 1:
                raise OSError("cannot start driver")
            return subprocess.run(command, **kwargs)

        try:
            status = self.run_all(run_command=run_command)
        except OSError as error:
            self.fail(f"the full-suite runner must record launch errors and continue: {error}")
        self.assertEqual(status, 1)
        self.assertEqual(len(commands), 2)
        report = self.read_summary()
        self.assertFalse(report["scripts"][0]["success"])
        self.assertIsNone(report["scripts"][0]["returncode"])
        self.assertIn("cannot start driver", report["scripts"][0]["error"])
        self.assertTrue(report["scripts"][1]["success"])
        self.assertIn("cannot start driver", (self.root / report["scripts"][0]["log"]).read_text())

    def test_a_discovery_exception_still_runs_the_step_scripts(self):
        self.write_suite("def load_tests(loader, tests, pattern): return object()\n")
        self.write_scripts(["a.steps", "b.steps"])
        try:
            status = self.run_all()
        except TypeError as error:
            self.fail(f"discovery must not terminate the full-suite runner: {error}")
        self.assertEqual(status, 1)
        report = self.read_summary()
        self.assertFalse(report["unittest"]["success"])
        self.assertIn("error", report["unittest"])
        self.assertEqual(len(report["scripts"]), 2)
        self.assertTrue(all(script["success"] for script in report["scripts"]))
        self.assertIn("TypeError", (self.tests / "out/run-all/unittest.log").read_text())

    def test_no_step_scripts_is_a_full_suite_failure(self):
        self.write_suite("class Fixture(unittest.TestCase):\n    def test_pass(self): pass\n")
        self.assertEqual(self.run_all(), 1)
        report = self.read_summary()
        self.assertTrue(report["unittest"]["success"])
        self.assertFalse(report["success"])
        self.assertEqual(report["scripts"], [])
        self.assertIn("no step scripts", self.output.getvalue())

    def test_cli_runs_from_another_directory_and_returns_the_aggregate_status(self):
        self.write_suite("class Fixture(unittest.TestCase):\n    def test_pass(self): pass\n")
        self.write_scripts(["a.steps", "b.steps"])
        runner_path = self.tests / "run_all.py"
        runner_path.write_text(self.runner_path.read_text(), encoding="utf-8")
        binary_directory = self.root / "bin"
        binary_directory.mkdir()
        for name in ("bash", "cargo", "tmux", "zellij"):
            executable = binary_directory / name
            executable.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            executable.chmod(0o755)
        environment = dict(os.environ, PATH=str(binary_directory))
        for script_status in (0, 9):
            with self.subTest(script_status=script_status):
                (self.tests / "scripts/a.steps").write_text(str(script_status))
                completed = subprocess.run(
                    [sys.executable, str(runner_path)], cwd=self.root.parent,
                    env=environment, capture_output=True, text=True, check=False,
                )
                self.assertEqual(completed.returncode, int(script_status != 0), completed.stderr)
                report = self.read_summary()
                self.assertEqual(report["success"], script_status == 0)
                self.assertEqual([script["returncode"] for script in report["scripts"]], [script_status, 0])

    def test_missing_executables_fail_before_running_any_tests(self):
        self.assertTrue(callable(getattr(self.runner_module, "main", None)))
        lookups = []

        def missing_executable(name, *, path):
            lookups.append(name)
            self.assertEqual(path.split(os.pathsep)[0], str(self.root / "target/debug"))
            return None

        def unexpected_command(*args, **kwargs):
            self.fail("preflight must not launch a command")

        status = self.runner_module.main(
            root=self.root, output=self.output,
            executable_lookup=missing_executable, run_command=unexpected_command,
        )
        self.assertEqual(status, 1)
        self.assertEqual(lookups, ["bash", "cargo", "tmux", "zellij"])
        report = json.loads((self.tests / "out/run-all/summary.json").read_text())
        self.assertFalse(report["success"])
        self.assertEqual(report["preflight"]["missing"], lookups)
        self.assertIsNone(report["unittest"])
        self.assertEqual(report["scripts"], [])
        for name in lookups:
            self.assertIn(name, self.output.getvalue())


if __name__ == "__main__":
    unittest.main()
