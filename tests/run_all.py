#!/usr/bin/env python3
"""Run every Python test and step script without accepting omitted tests.

This local command also runs in CI. Run Rust tests with cargo separately.
"""

import contextlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import traceback
import unittest


ROOT = Path(__file__).resolve().parent.parent
REQUIRED_EXECUTABLES = ("bash", "cargo", "tmux", "zellij")


def run_unittests(test_directory, output):
    suite = unittest.TestLoader().discover(str(test_directory), pattern="test*.py")
    discovered = suite.countTestCases()
    result = unittest.TextTestRunner(
        stream=output, verbosity=2, failfast=False
    ).run(suite)
    return {
        "success": (
            discovered > 0
            and result.testsRun == discovered
            and result.wasSuccessful()
            and not result.skipped
            and not result.expectedFailures
        ),
        "discovered": discovered,
        "failures": len(result.failures),
        "errors": len(result.errors),
        "expected_failures": len(result.expectedFailures),
        "unexpected_successes": len(result.unexpectedSuccesses),
        "tests_run": result.testsRun,
        "skipped": len(result.skipped),
    }


def main(
    *, root=ROOT, output=None, executable_lookup=shutil.which,
    run_command=subprocess.run,
):
    output = output if output is not None else sys.stdout
    root = Path(root).resolve()
    output_directory = root / "tests" / "out" / "run-all"
    output_directory.mkdir(parents=True, exist_ok=True)
    environment = dict(os.environ)
    environment["PATH"] = os.pathsep.join(
        (str(root / "target" / "debug"), environment.get("PATH", ""))
    )
    executables = {
        name: executable_lookup(name, path=environment["PATH"])
        for name in REQUIRED_EXECUTABLES
    }
    missing = [name for name, executable in executables.items() if executable is None]
    report = {
        "success": False,
        "preflight": {"executables": executables, "missing": missing},
        "unittest": None,
        "scripts": [],
    }
    if missing:
        print("FAILED: missing required executables: " + ", ".join(missing), file=output)
    else:
        environment["SHELL"] = str(Path(executables["bash"]).resolve())
        environment["PWD"] = str(root)
        environment["TERM"] = "xterm-256color"
        # A test launched inside a multiplexer must not inherit its location.
        for name in ("TMUX", "TMUX_PANE", "ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID"):
            environment.pop(name, None)
        socket_directory = tempfile.TemporaryDirectory(prefix="wrangler-test-")
        environment["TMUX_TMPDIR"] = socket_directory.name
        previous_directory = Path.cwd()
        previous_environment = dict(os.environ)
        previous_path = sys.path[:]
        try:
            os.chdir(root)
            os.environ.clear()
            os.environ.update(environment)
            unittest_log = output_directory / "unittest.log"
            with unittest_log.open("w", encoding="utf-8") as log:
                with contextlib.redirect_stdout(log), contextlib.redirect_stderr(log):
                    try:
                        report["unittest"] = run_unittests(root / "tests", log)
                    except Exception as error:
                        traceback.print_exc(file=log)
                        report["unittest"] = {"success": False, "error": str(error)}
            report["unittest"]["log"] = str(unittest_log.relative_to(root))
            status = "PASS" if report["unittest"]["success"] else "FAIL"
            print(f"unittest: {status}", file=output, flush=True)
            script_directory = root / "tests" / "scripts"
            for script in sorted(script_directory.rglob("*.steps")):
                if not script.is_file():
                    continue
                relative_script = script.relative_to(script_directory)
                script_log = output_directory / "scripts" / (str(relative_script) + ".log")
                script_log.parent.mkdir(parents=True, exist_ok=True)
                script_report = {
                    "script": str(script.relative_to(root)),
                    "log": str(script_log.relative_to(root)),
                    "returncode": None,
                    "success": False,
                }
                # Step assertions read tests/out directly. Keep the default outdir.
                with script_log.open("w", encoding="utf-8") as log:
                    try:
                        completed = run_command(
                            [sys.executable, str(root / "tests" / "drive.py"), str(script)],
                            cwd=root, env=environment, stdout=log, stderr=subprocess.STDOUT,
                            check=False,
                        )
                        script_report["returncode"] = completed.returncode
                        script_report["success"] = completed.returncode == 0
                    except OSError as error:
                        script_report["error"] = str(error)
                        print("FAILED: " + str(error), file=log)
                report["scripts"].append(script_report)
                status = "PASS" if script_report["success"] else "FAIL"
                print(f"{script_report['script']}: {status}", file=output, flush=True)
            if not report["scripts"]:
                print("FAILED: no step scripts discovered", file=output)
            report["success"] = (
                report["unittest"]["success"]
                and bool(report["scripts"])
                and all(script["success"] for script in report["scripts"])
            )
        finally:
            socket_directory.cleanup()
            os.chdir(previous_directory)
            os.environ.clear()
            os.environ.update(previous_environment)
            sys.path[:] = previous_path
    (output_directory / "summary.json").write_text(
        json.dumps(report, indent=2) + "\n", encoding="utf-8"
    )
    return 0 if report["success"] else 1


if __name__ == "__main__":
    sys.exit(main())
