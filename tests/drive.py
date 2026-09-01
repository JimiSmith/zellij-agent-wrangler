#!/usr/bin/env python3
"""Drives a program in a real pty and asserts on what it puts on screen.

A test script is a list of steps, one step per line. The steps come from a file
or from a list of strings. Each step has the form `name: argument`. The harness
ignores blank lines and lines that start with `#`. For the step reference, see
tests/README.md.

Every failure is loud. The harness prints the current screen and the tail of the
raw byte stream before the run exits with a non-zero status. A harness that
fails quietly costs more time than no harness at all.

Session safety, zellij: this harness only creates and deletes zellij sessions
with names that start with `wrangler-test-`. Cleanup uses the names that this
run mentioned. `guard_session_name` rejects every name outside the prefix, so the
harness never reaches the sessions of the developer.

Session safety, tmux: a tmux command must run against a server of the harness's
own. `guard_tmux_command` rejects a tmux command that does not carry
`-L wrangler-test`. That server is a separate process, so the sessions of the
developer are out of reach whatever a command says.
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

# Shell commands that can reach beyond the sessions that this harness created.
_FORBIDDEN = ("kill-all-sessions", "delete-all-sessions")

# The tmux server that this harness starts, and the only one it may reach.
TMUX_SERVER = "wrangler-test"
TMUX_SERVER_FLAG = "-L " + TMUX_SERVER

# A tmux command in a shell line. The match needs a word and not a substring:
# `target/debug/tmux-agent-wrangler` holds the letters and is not a tmux command.
# The group before the name accepts a command run by its path, such as
# `/usr/local/bin/tmux`. The lookahead after the name separates `tmux` from
# `tmux-agent-wrangler`. Both edges accept a quote, because a step can
# write `sh -c 'tmux ...'`.
_TMUX_COMMAND = re.compile(
    r"""(?:^|[\s;|&('"`])(?:[\w./-]*/)?tmux(?=[\s;|&)'"`]|$)"""
)

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
    """Turns a `keys:` argument into the bytes that go to the pty."""

    def control(match):
        return chr(ord(match.group(1).upper()) ^ 0x40)

    text = _CONTROL.sub(control, literal)
    out = []
    index = 0
    while index < len(text):
        # The loop tries `\\` first, so a literal backslash cannot swallow the
        # letter after it and become a different escape.
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
    """The names of the known zellij sessions. If zellij is absent, the list is
    empty."""
    if not which("zellij"):
        return []
    result = subprocess.run(
        ["zellij", "list-sessions", "--no-formatting", "--short"],
        capture_output=True,
        text=True,
    )
    # A non-zero exit only means that there were no sessions to list. The
    # explanation goes to stderr. This code therefore reads stdout in both
    # cases, and no leaked session survives cleanup.
    return [line.strip() for line in result.stdout.splitlines() if line.strip()]


class Pty:
    """A forked child on the far end of a pty, and the screen that it draws on."""

    def __init__(self, argv, rows=DEFAULT_ROWS, cols=DEFAULT_COLS, env=None):
        self.argv = argv
        self.screen = Screen(rows, cols)
        self.raw = bytearray()
        self.exit_status = None
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            # This code runs in the child. Nothing here must return into the
            # control flow of the parent. A failed exec therefore ends the
            # process and raises no exception.
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
        """Reads the available bytes and replays them. Returns true for new bytes."""
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
                # A pty reports the exit of the child as EIO and not as EOF.
                if exc.errno in (errno.EIO, errno.EBADF):
                    break
                raise
            if not chunk:
                break
            self.raw += chunk
            self.screen.feed(chunk)
            got = True
            # This code answers queries such as "report the cursor position".
            # Without an answer, a program that waits for one stalls.
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


def runs_tmux(text):
    """Tells whether a command line runs the tmux program.

    The tmux client of this project is called `tmux-agent-wrangler`. Its path
    holds the letters of `tmux` and it is not a tmux command, so a substring
    test refuses a command that is safe.
    """
    return _TMUX_COMMAND.search(text) is not None


def guard_tmux_command(text):
    """Refuses a tmux command that does not name the server of this harness.

    A tmux server is one process, and `-L` chooses which one. Without the flag,
    a command reaches the server that holds the sessions of the developer. That
    is the server that `kill-server` would end.
    """
    if runs_tmux(text) and TMUX_SERVER_FLAG not in text:
        raise StepFailure(
            "refusing to run %r: a tmux command must carry %r, so that it "
            "reaches the server of this harness and no other"
            % (text, TMUX_SERVER_FLAG)
        )
    return text


class Runner:
    def __init__(self, rows=DEFAULT_ROWS, cols=DEFAULT_COLS, outdir=None, verbose=True):
        self.rows = rows
        self.cols = cols
        here = os.path.dirname(os.path.abspath(__file__))
        self.outdir = outdir or os.path.join(here, "out")
        self.verbose = verbose
        self.pty = None
        self.sessions = []
        # Whether a command in this run started the tmux server of the harness.
        # Cleanup ends that server, and it ends nothing when no command ran tmux.
        self.started_tmux = False
        self.dumps = []
        # These variables go to every child that starts after the `env:` step
        # that set them. A script pins PS1 or TERM in this way.
        self.env = {}

    # -- lifecycle --------------------------------------------------------

    def spawn(self, command, env=None):
        if self.pty is not None:
            self.pty.close()
        argv = shlex.split(command)
        guard_tmux_command(command)
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
        """Records what a command mentions, so that cleanup knows what to end.

        Cleanup works from these records. The harness deletes a zellij session
        only because a command from this run named it. The harness ends the tmux
        server only because a command from this run ran tmux.
        """
        for name in _SESSION_NAME.findall(text):
            if name not in self.sessions:
                self.sessions.append(name)
        if runs_tmux(text):
            self.started_tmux = True

    def cleanup(self):
        if self.pty is not None:
            self.pty.close()
            self.pty = None
        self._end_tmux_server()
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

    def _end_tmux_server(self):
        """Ends the tmux server of this harness, and no other.

        `kill-server` is safe here because `-L` names one server, and that
        server holds only what this run started. The command is a list and not a
        shell line, so the server name cannot come from anywhere else.
        """
        if not self.started_tmux or not which("tmux"):
            return
        self._log("cleanup: ending tmux server %s" % TMUX_SERVER)
        subprocess.run(
            ["tmux", "-L", TMUX_SERVER, "kill-server"],
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
        # This code expands variables, so a step can name a path relative to the
        # start directory of the run. Some programs that the harness drives
        # resolve their own directories through the XDG rules. Those rules ignore
        # a value that is not an absolute path. Without a message, the program
        # then reads the real configuration of the developer instead of the
        # configuration of the test. That failure looks like a bug in the program
        # under test.
        self.env[name.strip()] = os.path.expandvars(value)

    def _step_sh(self, argument, must_succeed=True):
        for token in _FORBIDDEN:
            if token in argument:
                raise StepFailure(
                    "refusing to run %r: it would reach sessions this harness "
                    "did not create" % argument
                )
        guard_tmux_command(argument)
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
        # This code reads back the reaction to the write. The next `wait:` step
        # then starts from a screen that shows the reaction.
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
        """Sets the size of the terminal.

        Before a spawn this sets the size that the child is started at. A
        program that needs more room than the default gets it from the first
        byte it writes, rather than from a resize that it has to survive.

        After a spawn this resizes the child that is running.
        """
        rows, _, cols = argument.partition("x")
        self.rows = int(rows)
        self.cols = int(cols)
        child = self.pty
        if child is None:
            return
        child.screen.rows = self.rows
        child.screen.cols = self.cols
        # The reset builds the grid again at the new size. Everything from
        # before the resize is gone, and the child must draw the screen again.
        child.screen.reset()
        child.set_winsize(self.rows, self.cols)

    # -- helpers ----------------------------------------------------------

    def _split_timeout(self, argument):
        """Splits a trailing `@ <seconds>` timeout off the argument of a wait step."""
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
                # This is a last drain. The child can draw the text and exit
                # inside the same pump window.
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
    """Runs `steps` to the end and returns a process exit code."""
    runner = Runner(rows=rows, cols=cols, outdir=outdir, verbose=verbose)
    try:
        runner.run(steps)
    except StepFailure as failure:
        runner.report_failure(str(failure))
        runner.cleanup()
        return 1
    except Exception as failure:
        # An unexpected error still owes the reader the screen of the failure.
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
