#!/usr/bin/env python3
"""Drive a program in a real pty and assert on what it puts on screen.

A test script is a list of steps, one per line, read from a file or passed in
as strings. Each step is `name: argument`; blank lines and lines starting with
`#` are ignored. See tests/README.md for the step reference.

Every failure is loud: the current screen and the tail of the raw byte stream
are printed before the run exits non-zero, because a harness that fails quietly
costs more time than no harness at all.

Session safety: this harness only ever creates and deletes zellij sessions
whose names start with `wrangler-test-`. Cleanup works from the names this run
itself mentioned, and `_guard_session_name` rejects anything outside the
prefix, so a developer's own sessions are never in reach.
"""

import argparse
import errno
import fcntl
import os
import pty
import re
import select
import shlex
import signal
import struct
import subprocess
import sys
import termios
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from screen import Screen  # noqa: E402

SESSION_PREFIX = "wrangler-test-"
_SESSION_NAME = re.compile(r"wrangler-test-[A-Za-z0-9_.-]+")

# Shell commands that would reach beyond the sessions this harness created.
_FORBIDDEN = ("kill-all-sessions", "delete-all-sessions")

DEFAULT_ROWS = 24
DEFAULT_COLS = 80
DEFAULT_TIMEOUT = 10.0
TAIL_BYTES = 2048

_CONTROL = re.compile(r"<C-(.)>", re.IGNORECASE)
_ESCAPES = (
    ("\\\\", "\\"),
    ("\\r", "\r"),
    ("\\n", "\n"),
    ("\\e", "\x1b"),
    ("\\t", "\t"),
)


class StepFailure(Exception):
    pass


def unescape(literal):
    """Turn a `keys:` argument into the bytes to write to the pty."""

    def control(match):
        return chr(ord(match.group(1).upper()) ^ 0x40)

    text = _CONTROL.sub(control, literal)
    out = []
    index = 0
    while index < len(text):
        # `\\` is tried first, so a literal backslash cannot swallow the letter
        # after it and turn into a different escape.
        for token, value in _ESCAPES:
            if text.startswith(token, index):
                out.append(value)
                index += len(token)
                break
        else:
            out.append(text[index])
            index += 1
    return "".join(out).encode("utf-8")


def which(binary):
    for directory in os.environ.get("PATH", "").split(os.pathsep):
        candidate = os.path.join(directory, binary)
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    return None


def live_sessions():
    """Names of zellij sessions currently known, or [] if zellij is absent."""
    if not which("zellij"):
        return []
    result = subprocess.run(
        ["zellij", "list-sessions", "--no-formatting", "--short"],
        capture_output=True,
        text=True,
    )
    # A non-zero exit only means there were no sessions to list, and the
    # explanation goes to stderr, so stdout is read either way rather than
    # letting a leaked session survive cleanup.
    return [line.strip() for line in result.stdout.splitlines() if line.strip()]


class Pty:
    """A forked child on the far end of a pty, plus the screen it draws on."""

    def __init__(self, argv, rows=DEFAULT_ROWS, cols=DEFAULT_COLS, env=None):
        self.argv = argv
        self.screen = Screen(rows, cols)
        self.raw = bytearray()
        self.exit_status = None
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            # In the child. Nothing here may return into the parent's control
            # flow, so a failed exec ends the process rather than raising.
            try:
                if env is not None:
                    os.execvpe(argv[0], argv, env)
                else:
                    os.execvp(argv[0], argv)
            except Exception as exc:  # pragma: no cover - child process only
                sys.stderr.write("exec failed: %s\n" % exc)
            os._exit(127)
        self.set_winsize(rows, cols)

    def set_winsize(self, rows, cols):
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def alive(self):
        if self.exit_status is not None:
            return False
        pid, status = os.waitpid(self.pid, os.WNOHANG)
        if pid == 0:
            return True
        self.exit_status = status
        return False

    def write(self, data):
        os.write(self.fd, data)

    def pump(self, timeout=0.1):
        """Read whatever is available and replay it. True if bytes arrived."""
        got = False
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            try:
                ready, _, _ = select.select([self.fd], [], [], remaining)
            except (OSError, ValueError):
                break
            if not ready:
                break
            try:
                chunk = os.read(self.fd, 65536)
            except OSError as exc:
                # A pty reports the child's exit as EIO rather than as EOF.
                if exc.errno in (errno.EIO, errno.EBADF):
                    break
                raise
            if not chunk:
                break
            self.raw += chunk
            self.screen.feed(chunk)
            got = True
            # Queries such as "report the cursor position" are answered here.
            # A program that blocks on the answer stalls if this is skipped.
            reply = self.screen.take_replies()
            if reply:
                try:
                    self.write(reply)
                except OSError:
                    pass
        return got

    def close(self):
        if self.exit_status is None:
            for sig in (signal.SIGTERM, signal.SIGKILL):
                try:
                    os.kill(self.pid, sig)
                except ProcessLookupError:
                    break
                for _ in range(20):
                    if not self.alive():
                        break
                    time.sleep(0.05)
                if self.exit_status is not None:
                    break
        try:
            os.close(self.fd)
        except OSError:
            pass


def guard_session_name(name):
    if not name.startswith(SESSION_PREFIX):
        raise StepFailure(
            "refusing to touch zellij session %r: this harness only handles "
            "names starting with %r" % (name, SESSION_PREFIX)
        )
    return name


class Runner:
    def __init__(self, rows=DEFAULT_ROWS, cols=DEFAULT_COLS, outdir=None, verbose=True):
        self.rows = rows
        self.cols = cols
        here = os.path.dirname(os.path.abspath(__file__))
        self.outdir = outdir or os.path.join(here, "out")
        self.verbose = verbose
        self.pty = None
        self.sessions = []
        self.dumps = []
        # Applied to every child spawned after the `env:` step that set them,
        # which is how a script pins things like PS1 or TERM.
        self.env = {}

    # -- lifecycle --------------------------------------------------------

    def spawn(self, command, env=None):
        if self.pty is not None:
            self.pty.close()
        argv = shlex.split(command)
        self._note_sessions(command)
        child_env = dict(os.environ)
        child_env.setdefault("TERM", "xterm-256color")
        child_env["LINES"] = str(self.rows)
        child_env["COLUMNS"] = str(self.cols)
        child_env.update(self.env)
        if env:
            child_env.update(env)
        self.pty = Pty(argv, self.rows, self.cols, child_env)

    def _note_sessions(self, text):
        """Record every `wrangler-test-` name a command mentions.

        Cleanup works from this list, so a session is only ever deleted because
        a command this run issued named it.
        """
        for name in _SESSION_NAME.findall(text):
            if name not in self.sessions:
                self.sessions.append(name)

    def cleanup(self):
        if self.pty is not None:
            self.pty.close()
            self.pty = None
        if not self.sessions or not which("zellij"):
            return
        live = set(live_sessions())
        for name in self.sessions:
            guard_session_name(name)
            if name not in live:
                continue
            self._log("cleanup: deleting zellij session %s" % name)
            subprocess.run(
                ["zellij", "delete-session", name, "--force"],
                capture_output=True,
                text=True,
            )

    # -- steps ------------------------------------------------------------

    def run(self, steps):
        for index, raw in enumerate(steps, start=1):
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            if ":" not in line:
                raise StepFailure("line %d: no step name in %r" % (index, raw))
            name, _, argument = line.partition(":")
            name = name.strip()
            argument = argument.strip()
            self._log("[%02d] %s: %s" % (index, name, argument))
            handler = getattr(self, "_step_" + name.replace("?", "_opt"), None)
            if handler is None:
                raise StepFailure("line %d: unknown step %r" % (index, name))
            handler(argument)

    def _need_pty(self, name):
        if self.pty is None:
            raise StepFailure("step %r needs a running child: use `spawn:` first" % name)
        return self.pty

    def _step_spawn(self, argument):
        self.spawn(argument)

    def _step_env(self, argument):
        name, sep, value = argument.partition("=")
        if not sep:
            raise StepFailure("env step wants NAME=value, got %r" % argument)
        # Variables are expanded so a step can name a path relative to where the
        # run started. Some of what the harness drives resolves its own
        # directories through the XDG rules, which ignore a value that is not an
        # absolute path, and silently reading the developer's real configuration
        # instead of the test's is the kind of failure that looks like a bug in
        # what is being tested.
        self.env[name.strip()] = os.path.expandvars(value)

    def _step_sh(self, argument, must_succeed=True):
        for token in _FORBIDDEN:
            if token in argument:
                raise StepFailure(
                    "refusing to run %r: it would reach sessions this harness "
                    "did not create" % argument
                )
        self._note_sessions(argument)
        result = subprocess.run(argument, shell=True, capture_output=True, text=True)
        if result.stdout.strip():
            self._log("    stdout: %s" % result.stdout.strip())
        if result.stderr.strip():
            self._log("    stderr: %s" % result.stderr.strip())
        if must_succeed and result.returncode != 0:
            raise StepFailure(
                "shell command failed (%d): %s" % (result.returncode, argument)
            )

    def _step_sh_opt(self, argument):
        self._step_sh(argument, must_succeed=False)

    def _step_keys(self, argument):
        child = self._need_pty("keys")
        child.write(unescape(argument))
        # Read back whatever the write provoked, so a following `wait:` starts
        # from a screen that has already seen the reaction.
        child.pump(0.2)

    def _step_wait(self, argument):
        needle, timeout = self._split_timeout(argument)
        self._pump_until(
            lambda: self.pty.screen.contains(needle),
            timeout,
            "waiting for %r on screen" % needle,
        )

    def _step_waitgone(self, argument):
        needle, timeout = self._split_timeout(argument)
        self._pump_until(
            lambda: not self.pty.screen.contains(needle),
            timeout,
            "waiting for %r to leave the screen" % needle,
        )

    def _step_sleep(self, argument):
        deadline = time.monotonic() + float(argument)
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return
            if self.pty is None:
                time.sleep(min(0.2, remaining))
            else:
                self.pty.pump(min(0.2, remaining))

    def _step_dump(self, argument):
        self.dump(argument)

    def _step_resize(self, argument):
        rows, _, cols = argument.partition("x")
        child = self._need_pty("resize")
        self.rows = int(rows)
        self.cols = int(cols)
        child.screen.rows = self.rows
        child.screen.cols = self.cols
        # The grid is rebuilt at the new size, so everything drawn before the
        # resize is gone and the child is expected to redraw.
        child.screen.reset()
        child.set_winsize(self.rows, self.cols)

    # -- helpers ----------------------------------------------------------

    def _split_timeout(self, argument):
        """Split a trailing `@ <seconds>` timeout off a wait step's argument."""
        if "@" in argument:
            needle, _, tail = argument.rpartition("@")
            try:
                return needle.strip(), float(tail.strip())
            except ValueError:
                pass
        return argument, DEFAULT_TIMEOUT

    def _pump_until(self, predicate, timeout, description):
        child = self._need_pty("wait")
        deadline = time.monotonic() + timeout
        while True:
            child.pump(0.1)
            if predicate():
                return
            if time.monotonic() >= deadline:
                raise StepFailure("timed out after %.1fs %s" % (timeout, description))
            if not child.alive():
                # A last drain: the child can draw the thing and exit within
                # the same pump window.
                child.pump(0.2)
                if predicate():
                    return
                raise StepFailure(
                    "child exited (status %s) while %s"
                    % (child.exit_status, description)
                )

    def dump(self, name):
        child = self._need_pty("dump")
        child.pump(0.2)
        os.makedirs(self.outdir, exist_ok=True)
        text_path = os.path.join(self.outdir, name + ".txt")
        sgr_path = os.path.join(self.outdir, name + ".sgr.json")
        with open(text_path, "w", encoding="utf-8") as handle:
            handle.write(child.screen.dump_text() + "\n")
        with open(sgr_path, "w", encoding="utf-8") as handle:
            handle.write(child.screen.dump_sgr_json() + "\n")
        self.dumps.append(text_path)
        self._log("    dumped %s and %s" % (text_path, sgr_path))
        return text_path, sgr_path

    def _log(self, message):
        if self.verbose:
            print(message, flush=True)

    def report_failure(self, message):
        out = sys.stderr
        out.write("\nFAILED: %s\n" % message)
        if self.pty is None:
            out.write("(no child was running)\n")
            out.flush()
            return
        screen = self.pty.screen
        out.write("\n--- screen (%d rows by %d cols) ---\n" % (screen.rows, screen.cols))
        for index, line in enumerate(screen.text()):
            out.write("%3d|%s\n" % (index, line))
        out.write("--- end screen ---\n")
        out.write(
            "unhandled sequences: %d %s\n"
            % (screen.unhandled, screen.unhandled_summary())
        )
        tail = bytes(self.pty.raw[-TAIL_BYTES:])
        out.write("\n--- last %d raw bytes ---\n%r\n--- end raw ---\n" % (len(tail), tail))
        out.flush()


def run_script(steps, rows=DEFAULT_ROWS, cols=DEFAULT_COLS, outdir=None, verbose=True):
    """Run `steps` to completion and return a process exit code."""
    runner = Runner(rows=rows, cols=cols, outdir=outdir, verbose=verbose)
    try:
        runner.run(steps)
    except StepFailure as failure:
        runner.report_failure(str(failure))
        runner.cleanup()
        return 1
    except Exception as failure:
        # An unexpected error still owes the reader the screen it happened on.
        runner.report_failure("%s: %s" % (type(failure).__name__, failure))
        runner.cleanup()
        raise
    runner.cleanup()
    if verbose:
        print("OK: all steps completed", flush=True)
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description="Drive a program in a real pty.")
    parser.add_argument("script", help="file of steps, or - to read stdin")
    parser.add_argument("--rows", type=int, default=DEFAULT_ROWS)
    parser.add_argument("--cols", type=int, default=DEFAULT_COLS)
    parser.add_argument("--outdir", default=None)
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args(argv)

    if args.script == "-":
        steps = sys.stdin.read().splitlines()
    else:
        with open(args.script, encoding="utf-8") as handle:
            steps = handle.read().splitlines()

    return run_script(
        steps,
        rows=args.rows,
        cols=args.cols,
        outdir=args.outdir,
        verbose=not args.quiet,
    )


if __name__ == "__main__":
    sys.exit(main())
