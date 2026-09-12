"""Real-tmux ownership regressions, with the existing PTY/screen emulator.

Run from the repository root after the native build:
    python3 -m unittest discover -s tests -p test_tmux_exclusion.py -v
Every command targets this test's private server. No developer session or daemon
is touched. Pane captures assert membership; the attached PTY proves focus/input.
"""
from pathlib import Path
import json
import os
import shlex
import shutil
import signal
import subprocess
import sys
import time
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from drive import Pty

ROOT = Path(__file__).resolve().parents[1]
SIDEBAR = ROOT / "target/debug/tmux-agent-wrangler"
CLIENT = ROOT / "target/debug/agent-wrangler"
OPTION = "@agent-wrangler-sidebar"
MODES = {
    "tree": [],
    "sections": ["--sections", "true"],
    "dashboard": ["--dashboard", "true"],
    "both": ["--dashboard", "true", "--sections", "true"],
}


@unittest.skipUnless(shutil.which("tmux") and sys.platform.startswith("linux"),
                     "live owner/process assertions require Linux and tmux")
class TestTmuxExclusion(unittest.TestCase):
    def setUp(self):
        self.assertTrue(SIDEBAR.exists(), "build the native sidebar first")
        self.server = f"wrangler-test-exclusion-{os.getpid()}"
        self.session = "wrangler-test-exclusion"
        self.out = ROOT / "tests/out/exclusion" / self._testMethodName
        self.out.mkdir(parents=True, exist_ok=True)
        self.env = os.environ.copy()
        self.env.pop("TMUX", None)
        self.env.pop("TMUX_PANE", None)
        self.env.update(USER=self.server, TERM="xterm-256color",
                        XDG_STATE_HOME=str(self.out / "state"),
                        PATH=str(ROOT / "target/debug") + os.pathsep + self.env["PATH"])
        self.commands = []
        self.pty = None
        self.started = False
        existing = subprocess.run(["tmux", "-L", self.server, "has-session"],
                                  env=self.env, capture_output=True, timeout=10)
        self.assertNotEqual(existing.returncode, 0, "refuse an existing test server")
        self.addCleanup(self.cleanup)
        self.content = self.tmux("-f", "/dev/null", "new-session", "-d", "-s", self.session,
                                 "-n", "CONTENT", "-x", "240", "-y", "55", "-P", "-F", "#{pane_id}",
                                 "bash --noprofile --norc")
        self.started = True
        self.tmux("set-option", "-g", "status", "off")
        self.tmux("select-pane", "-t", self.content, "-T", "KEEP-PLAIN")
        self.pty = Pty(["tmux", "-L", self.server, "attach-session", "-t", self.session],
                       rows=55, cols=240, env=self.env)
        self.pty.pump(0.2)

    def tmux(self, *args, check=True):
        command = ["tmux", "-L", self.server, *args]
        result = subprocess.run(command, env=self.env, text=True, capture_output=True, timeout=10)
        self.commands.append({"argv": command, "exit": result.returncode,
                              "stdout": result.stdout, "stderr": result.stderr})
        if check:
            self.assertEqual(result.returncode, 0, self.commands[-1])
        return result.stdout.rstrip("\r\n")

    def wait(self, predicate, message):
        deadline = time.monotonic() + 12
        while time.monotonic() < deadline:
            if self.pty:
                self.pty.pump(0.05)
            result = predicate()
            if result:
                return result
            time.sleep(0.05)
        self.fail(message)

    def capture(self, pane):
        return self.tmux("capture-pane", "-p", "-t", pane)

    def marker(self, pane):
        return self.tmux("show-option", "-pqv", "-t", pane, OPTION)

    def launch_command(self, mode):
        return shlex.join(["env", f"USER={self.env['USER']}", f"XDG_STATE_HOME={self.env['XDG_STATE_HOME']}",
                           f"PATH={self.env['PATH']}", str(SIDEBAR), "--label", "dir", *MODES[mode]]) + "; printf 'SIDEBAR-EXIT\\n'; sleep 600"

    def launch(self, mode="tree", target=None, pane=None):
        if pane:
            self.tmux("respawn-pane", "-k", "-t", pane, self.launch_command(mode))
        else:
            pane = self.tmux("split-window", "-d", "-h", "-l", "75", "-t", target or self.content,
                             "-P", "-F", "#{pane_id}", self.launch_command(mode))
        self.wait(lambda: self.marker(pane).startswith(f"v1:{pane}:"), "sidebar did not register")
        self.wait(lambda: self.tmux("display-message", "-p", "-t", pane, "#{alternate_on}") == "1",
                  "sidebar did not take the terminal")
        return pane

    def focus(self, pane):
        assert self.pty is not None
        self.tmux("select-window", "-t", pane)
        self.tmux("select-pane", "-t", pane)
        self.pty.pump(0.2)

    def quit(self, pane, key="q"):
        self.tmux("send-keys", "-t", pane, key)
        self.wait(lambda: "SIDEBAR-EXIT" in self.capture(pane), "sidebar did not quit")
        self.wait(lambda: not self.marker(pane), "normal quit retained its marker")

    def hook(self, name, pane, event="working", label=None):
        env = self.env.copy()
        env["TMUX"] = self.tmux("display-message", "-p", "-t", pane, "#{socket_path},#{pid},#{session_id}")
        env["TMUX_PANE"] = pane
        result = subprocess.run([str(CLIENT), "hook", "claude", event],
                                input=json.dumps({"session_id": name, "cwd": "/fixture/" + (label or name)}),
                                env=env, text=True, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)

    def save_screen(self, label):
        assert self.pty is not None
        self.pty.pump(0.2)
        (self.out / (label + ".txt")).write_text(self.pty.screen.dump_text())
        (self.out / (label + ".sgr.json")).write_text(self.pty.screen.dump_sgr_json())
        self.assertEqual(self.pty.screen.unhandled, 0, self.pty.screen.unhandled_summary())

    def cleanup(self):
        if self.pty:
            self.pty.close()
        if self.started:
            self.tmux("kill-server", check=False)
            result = subprocess.run(["tmux", "-L", self.server, "has-session"],
                                    env=self.env, capture_output=True, timeout=10)
            self.assertNotEqual(result.returncode, 0, "test server survived cleanup")
        # Only our executable AND our isolated daemon user qualify for cleanup.
        for proc in Path("/proc").iterdir():
            if not proc.name.isdigit():
                continue
            try:
                args = (proc / "cmdline").read_bytes().split(b"\0")
                env = (proc / "environ").read_bytes().split(b"\0")
                if args[0] == str(CLIENT).encode() and b"daemon" in args[1:] and ("USER=" + self.server).encode() in env:
                    os.kill(int(proc.name), signal.SIGTERM)
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                pass
        (self.out / "commands.json").write_text(json.dumps(self.commands, indent=2))

    def turns(self):
        result = subprocess.run([str(CLIENT), "agents"], env=self.env,
                                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        records = [line.split("\t") for line in result.stdout.splitlines()]
        return {fields[1]: fields[5] for fields in records if len(fields) > 5}

    def test_all_modes_exclude_every_peer_in_control_and_polling(self):
        peers = {}
        for mode in MODES:
            normal = self.content if not peers else self.tmux(
                "new-window", "-d", "-t", self.session, "-n", mode, "-P", "-F", "#{pane_id}", "sleep 600")
            peers[mode] = self.launch(mode, target=normal)
        self.hook("KEEP-AGENT", self.content)
        for mode, pane in peers.items():
            self.hook("SIDE-" + mode, pane, event="needsAttention")
        for phase in ["control", "polling"]:
            if phase == "polling":
                clients = self.tmux("list-clients", "-F", "#{client_name}\t#{client_control_mode}")
                self.assertEqual(sum(line.endswith("\t1") for line in clients.splitlines()), len(peers))
                for line in clients.splitlines():
                    name, control = line.split("\t")
                    if control == "1":
                        self.tmux("detach-client", "-t", name)
                self.tmux("rename-window", "-t", self.content, "POLLING-READY")
                self.wait(lambda: "POLLING-READY" in self.capture(peers["tree"]), "polling did not update")
            for mode, pane in peers.items():
                self.wait(lambda pane=pane: "KEEP-AGENT" in self.capture(pane), "positive control absent")
                self.wait(lambda pane=pane: "SIDE-" not in self.capture(pane), "sidebar agent leaked")
                text = self.capture(pane)
                (self.out / f"{phase}-{mode}.txt").write_text(text)
                self.assertNotIn("SIDE-", text)
                self.assertIn("KEEP-AGENT", text)
                self.focus(pane)
                self.save_screen(f"{phase}-{mode}-pty")
        # The authoritative peer record must return without a fresh hook event.
        self.quit(peers["sections"])
        self.wait(lambda: "SIDE-sections" in self.capture(peers["tree"]), "retained record did not return")

    def test_same_mode_pairs_and_all_restart_transitions(self):
        first = second = None
        for mode in MODES:
            first = self.launch(mode, pane=first)
            second = self.launch(mode, target=self.content, pane=second)
            self.hook("SIDE-first", first)
            self.hook("SIDE-second", second)
            self.hook("KEEP-AGENT", self.content)
            for pane in [first, second]:
                self.wait(lambda pane=pane: "KEEP-AGENT" in self.capture(pane) and "SIDE-" not in self.capture(pane), "same-mode exclusion failed")
            self.quit(first)
            self.quit(second)
        # Every directed transition between the three presentation modes.
        for before, after in [(a, b) for a in ["tree", "sections", "dashboard"]
                              for b in ["tree", "sections", "dashboard"] if a != b]:
            for mode in [before, after]:
                first = self.launch(mode, pane=first)
                self.assertTrue(self.marker(first).startswith(f"v1:{first}:"))
                self.quit(first)
        for key in ["Q", "C-c", "C-q"]:
            first = self.launch("tree", pane=first)
            self.quit(first, key)

    def test_stopped_and_crashed_shell_child_releases_reused_pane(self):
        assert self.pty is not None
        observer = self.launch()
        peer = self.launch("sections", target=self.content)
        self.tmux("select-pane", "-t", peer, "-T", "PEER-PANE")
        self.wait(lambda: "PEER-PANE" not in self.capture(observer), "peer visible before stop")
        marker = self.marker(peer)
        pid = int(marker.split(":")[2])
        self.assertNotEqual(pid, int(self.tmux("display-message", "-p", "-t", peer, "#{pane_pid}")), "must test a shell child")
        os.kill(pid, signal.SIGSTOP)
        try:
            self.pty.pump(1.2)
            self.assertNotIn("PEER-PANE", self.capture(observer))
            self.assertEqual(self.marker(peer), marker)
        finally:
            os.kill(pid, signal.SIGCONT)
        os.kill(pid, signal.SIGKILL)
        self.wait(lambda: "SIDEBAR-EXIT" in self.capture(peer), "shell did not survive its child")
        self.assertEqual(self.marker(peer), marker, "the crash oracle needs a stale option")
        self.wait(lambda: "PEER-PANE" in self.capture(observer), "dead owner still hides shell")
        self.tmux("respawn-pane", "-k", "-t", peer, "sleep 600")
        self.tmux("select-pane", "-t", peer, "-T", "REUSED-CONTENT")
        self.wait(lambda: "REUSED-CONTENT" in self.capture(observer), "respawn stays hidden")
        peer = self.launch("dashboard", pane=peer)
        self.assertNotEqual(self.marker(peer), marker)
        self.quit(peer)

    def test_inherited_malformed_and_mismatched_markers_do_not_hide_content(self):
        assert self.pty is not None
        observer = self.launch()
        live_marker = self.marker(observer)
        self.tmux("set-option", "-g", OPTION, live_marker)
        self.wait(lambda: "KEEP-PLAIN" in self.capture(observer), "inherited marker hid sibling")
        fields = live_marker.split(":")
        fields[1] = self.content
        fields[3] = str(int(fields[3]) + 1)
        bad_values = ["true", "v2:%0:1:1:1", "v1:%0:0:0:0", "v1:%0:42:1:1;kill-server", ":".join(fields)]
        for marker in bad_values:
            self.tmux("set-option", "-p", "-t", self.content, OPTION, marker)
            self.pty.pump(0.7)
            self.assertIn("KEEP-PLAIN", self.capture(observer), marker)
        self.tmux("set-option", "-pu", "-t", self.content, OPTION)
        self.tmux("set-option", "-gu", OPTION)
        # Misleading command/title text alone is never sidebar identity.
        self.tmux("select-pane", "-t", self.content, "-T", "tmux-agent-wrangler")
        self.wait(lambda: "tmux-agent-wrangler" in self.capture(observer), "title match hid ordinary pane")

    def test_delimiters_in_markers_cannot_hide_ordinary_panes(self):
        assert self.pty is not None
        observer = self.launch()
        self.tmux("split-window", "-d", "-v", "-t", self.content, "sleep 600")
        fields = self.marker(observer).split(":")
        fields[1] = self.content
        prefix = ":".join(fields)
        self.wait(lambda: "KEEP-PLAIN" in self.capture(observer), "positive control absent")
        for phase in ["control", "polling"]:
            if phase == "polling":
                clients = self.tmux("list-clients", "-F", "#{client_name}\t#{client_control_mode}")
                for line in clients.splitlines():
                    name, control = line.split("\t")
                    if control == "1":
                        self.tmux("detach-client", "-t", name)
            for suffix in ["\tjunk", "\n", "\njunk", "\r", "\x1b[31m", "\\tab"]:
                with self.subTest(phase=phase, suffix=repr(suffix)):
                    self.tmux("set-option", "-p", "-t", self.content, OPTION, prefix + suffix)
                    title = "FRAMING-" + str(len(self.commands))
                    self.tmux("rename-window", "-t", self.content, title)
                    self.wait(lambda: title in self.capture(observer), "topology did not observe the marker change")
                    self.assertIn("KEEP-PLAIN", self.capture(observer), "malformed marker hid ordinary content")
            self.tmux("set-option", "-pu", "-t", self.content, OPTION)
            self.wait(lambda: "KEEP-PLAIN" in self.capture(observer), "ordinary pane failed to return")

    def test_startup_failure_cleans_and_existing_owner_cannot_be_replaced(self):
        env = self.env.copy()
        env["TMUX"] = self.tmux("display-message", "-p", "-t", self.content, "#{socket_path},#{pid},#{session_id}")
        env["TMUX_PANE"] = self.content
        for args in [["--help"], ["--dashboard", "invalid"], []]:
            result = subprocess.run([str(SIDEBAR), *args], env=env, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0 if args == ["--help"] else 1)
            self.assertFalse(self.marker(self.content), "startup/help left a marker")
        observer = self.launch()
        marker = self.marker(observer)
        env["TMUX_PANE"] = observer
        result = subprocess.run([str(SIDEBAR)], env=env, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 1)
        self.assertIn(b"live sidebar already owns", result.stderr)
        self.assertEqual(self.marker(observer), marker)
        # An old guard cannot erase a replacement value when it notices loss.
        fields = marker.split(":")
        fields[-1] = str(int(fields[-1]) + 1)
        replacement = ":".join(fields)
        self.tmux("set-option", "-p", "-t", observer, OPTION, replacement)
        self.wait(lambda: "SIDEBAR-EXIT" in self.capture(observer), "ownership loss did not stop old drawing")
        self.assertEqual(self.marker(observer), replacement)

    def test_peer_focus_never_becomes_content_and_activation_keeps_ordinary_pane(self):
        assert self.pty is not None
        first = self.launch()
        second = self.launch("sections", target=self.content)
        self.hook("SIDE-first", first, "needsAttention")
        self.hook("SIDE-second", second, "needsAttention")
        self.focus(first)
        self.wait(lambda: "SIDE-" not in self.capture(first) and "KEEP-PLAIN" in self.capture(first), "peer leaked before activation")
        self.pty.write(b"j\r")
        self.wait(lambda: self.tmux("display-message", "-p", "-t", self.session, "#{pane_id}") == self.content, "activation selected a sidebar")
        self.pty.write(b"printf 'ACTIVATED-CONTENT\\n'\r")
        self.wait(lambda: self.pty.screen.contains("ACTIVATED-CONTENT"), "input missed ordinary pane")
        self.focus(second)
        self.save_screen("peer-focused")
        self.assertNotIn("SIDE-", self.capture(first))
        self.assertNotIn("SIDE-", self.capture(second))
        self.assertEqual(self.turns()["SIDE-first"], "attention")
        self.assertEqual(self.turns()["SIDE-second"], "attention")
        self.quit(first)
        self.wait(lambda: "SIDE-first" in self.capture(second), "peer focus falsely acknowledged its call")

    def test_no_peer_keeps_the_existing_last_content_close(self):
        observer = self.launch()
        self.focus(observer)
        self.wait(lambda: "KEEP-PLAIN" in self.capture(observer), "content never drawn")
        self.tmux("kill-pane", "-t", self.content)
        self.wait(lambda: "SIDEBAR-EXIT" in self.capture(observer), "single sidebar did not auto-close")
        self.assertFalse(self.marker(observer))

    def test_registration_in_last_content_pane_does_not_close_its_peer(self):
        assert self.pty is not None
        observer = self.launch()
        self.focus(observer)
        self.wait(lambda: "KEEP-PLAIN" in self.capture(observer), "initial content absent")
        peer = self.launch("sections", pane=self.content)
        self.focus(observer)
        self.pty.pump(1.0)
        self.assertTrue(self.marker(observer), "peer registration caused implicit close")
        self.assertTrue(self.marker(peer))
        self.quit(observer)
        self.assertTrue(self.marker(peer))

    def test_moved_and_renumbered_peer_keeps_its_stable_identity(self):
        observer = self.launch()
        normal = self.tmux("new-window", "-d", "-t", self.session, "-n", "other", "-P", "-F", "#{pane_id}", "sleep 600")
        peer = self.launch("dashboard", target=normal)
        marker = self.marker(peer)
        self.tmux("select-pane", "-t", peer, "-T", "MOVED-SIDEBAR")
        self.tmux("join-pane", "-d", "-h", "-s", peer, "-t", self.content)
        self.assertEqual(self.marker(peer), marker)
        self.tmux("move-window", "-s", self.content, "-t", self.session + ":7")
        self.wait(lambda: "7: CONTENT" in self.capture(observer), "window renumber was not observed")
        self.assertNotIn("MOVED-SIDEBAR", self.capture(observer))
        self.quit(peer)
        self.wait(lambda: "MOVED-SIDEBAR" in self.capture(observer), "moved pane did not return to content")

    def test_peer_only_panes_stay_alive_but_explicit_quit_exits(self):
        assert self.pty is not None
        first = self.launch()
        second = self.launch("sections", target=self.content)
        self.focus(first)
        self.wait(lambda: "KEEP-PLAIN" in self.capture(first), "ordinary pane missing")
        self.tmux("kill-pane", "-t", self.content)
        self.focus(first)
        self.pty.pump(1.2)
        self.assertTrue(self.marker(first), "first sidebar closed despite its physical peer")
        self.assertTrue(self.marker(second), "second sidebar closed despite its physical peer")
        self.assertNotIn("SIDEBAR-EXIT", self.capture(first))
        # Agent and focus inputs also reach the implicit-close effect gate.
        self.hook("SIDE-first", first, "needsAttention")
        self.focus(second)
        self.hook("SIDE-second", second, "needsAttention")
        self.focus(first)
        self.pty.pump(0.7)
        self.assertTrue(self.marker(first))
        self.assertTrue(self.marker(second))
        self.save_screen("two-sidebars-only")
        self.quit(first)
        self.assertTrue(self.marker(second), "explicit quit closed the peer too")


if __name__ == "__main__":
    unittest.main()
