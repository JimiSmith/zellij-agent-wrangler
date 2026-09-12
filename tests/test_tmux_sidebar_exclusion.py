"""Attached-PTY regression tests for pane-local display exclusion.

Run sequentially with the other tmux tests: all use -L wrangler-test.
Build the native binaries before running this module.
"""
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import time
import unittest

from drive import Pty

ROOT = Path(__file__).resolve().parents[1]
FLAG = "@agent-wrangler-sidebar"


@unittest.skipUnless(shutil.which("tmux"), "tmux not installed")
class TestTmuxSidebarExclusion(unittest.TestCase):
    def tmux(self, *args, check=True):
        result = subprocess.run(
            ["tmux", "-L", "wrangler-test", *args], cwd=ROOT,
            env=self.env, text=True, capture_output=True, timeout=10,
        )
        if check:
            self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.strip()

    def setUp(self):
        self.out = ROOT / "tests/out/sidebar-exclusion" / self._testMethodName
        self.out.mkdir(parents=True, exist_ok=True)
        # The sink name hashes the tmux socket, not USER. A separate daemon
        # user alone cannot isolate it from another harness daemon's sink.
        self.tmux_dir = tempfile.TemporaryDirectory(prefix="wrangler-test-")
        self.addCleanup(self.tmux_dir.cleanup)
        self.env = dict(os.environ, TERM="xterm-256color", TMUX_TMPDIR=self.tmux_dir.name,
                        USER=f"wrangler-test-exclusion-{os.getpid()}",
                        XDG_STATE_HOME=str(self.out / "state"),
                        PATH=str(ROOT / "target/debug") + os.pathsep + os.environ["PATH"])
        self.env.pop("TMUX", None)
        self.env.pop("TMUX_PANE", None)
        self.tmux("kill-server", check=False)
        self.daemon_log = (self.out / "daemon.log").open("w")
        self.daemon = subprocess.Popen(
            [str(ROOT / "target/debug/agent-wrangler"), "daemon"],
            env=self.env, stdout=self.daemon_log, stderr=subprocess.STDOUT,
        )
        self.terminal = Pty(
            ["tmux", "-L", "wrangler-test", "-f", "/dev/null", "new-session",
             "-s", "wrangler-test-exclusion", "-n", "PRIMARY", "bash --noprofile --norc"],
            rows=45, cols=280, env=self.env,
        )
        self.wait_until(lambda: bool(self.tmux("has-session", check=False) == "")
                        and bool(self.tmux("list-panes", "-F", "#{pane_id}", check=False)))
        self.content = self.tmux("display-message", "-p", "#{pane_id}")
        self.tmux("set-option", "-g", "allow-rename", "off")
        self.tmux("set-option", "-g", "automatic-rename", "off")
        self.tmux("select-pane", "-t", self.content, "-T", "tmux-agent-wrangler")

    def tearDown(self):
        self.dump("final")
        self.tmux("kill-server", check=False)
        self.terminal.close()
        self.daemon.terminate()
        try:
            self.daemon.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.daemon.kill()
            self.daemon.wait(timeout=5)
        self.daemon_log.close()

    def wait_until(self, predicate, timeout=15):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.terminal.pump(0.1)
            if predicate():
                return
        self.dump("failure")
        self.fail("condition timed out; see " + str(self.out))

    def settle(self, seconds=1.5):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self.terminal.pump(0.1)

    def dump(self, name):
        (self.out / (name + ".txt")).write_text("\n".join(self.terminal.screen.text()))
        (self.out / (name + ".raw")).write_bytes(self.terminal.raw)
        runs = [dict(row=r.row, col=r.col, text=r.text,
                     fg=r.sgr.fg, bg=r.sgr.bg) for r in self.terminal.screen.runs()]
        (self.out / (name + ".sgr.json")).write_text(json.dumps({
            "runs": runs, "unhandled": self.terminal.screen.unhandled}, indent=2))

    def flag(self, pane):
        return self.tmux("show-options", "-pqv", "-t", pane, FLAG, check=False)

    def screen(self, pane):
        return self.tmux("capture-pane", "-p", "-t", pane)

    def start_sidebar(self, mode, target=None, root=False):
        pane = self.tmux("split-window", "-h", "-l", "65", "-t", target or self.content,
                         "-P", "-F", "#{pane_id}", "bash --noprofile --norc")
        self.launch(pane, mode, root)
        return pane

    def launch(self, pane, mode, root=False):
        args = [str(ROOT / "target/debug/tmux-agent-wrangler")]
        if mode != "tree":
            args += ["--" + mode, "true"]
        command = shlex.join(args)
        if root:
            command = "exec " + command
        self.tmux("send-keys", "-t", pane, command, "Enter")
        self.wait_until(lambda: self.flag(pane) == "1")
        self.wait_until(lambda: any(label in self.screen(pane) for label in
                        (["no agents", "VISIBLE-AGENT"] if mode == "dashboard"
                         else ["PRIMARY", "POLLING"])))

    def call(self, pane, label):
        transcript = self.out / (label + ".jsonl")
        transcript.write_text(json.dumps({"type": "custom-title", "customTitle": label}) + "\n" +
            json.dumps({"type": "assistant", "message": {"model": "claude-opus-5",
                "content": [{"type": "text", "text": label + "-PREVIEW"}]}}) + "\n")
        env = dict(self.env, TMUX_PANE=pane, TMUX=self.tmux(
            "display-message", "-p", "-t", pane, "#{socket_path},#{pid},#{session_id}"))
        result = subprocess.run([str(ROOT / "target/debug/agent-wrangler"),
            "hook", "claude", "needsAttention"], env=env, text=True, capture_output=True,
            input=json.dumps({"session_id": label, "cwd": str(ROOT),
                              "transcript_path": str(transcript)}), timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.wait_until(lambda: self.turns().get(label) == "attention")

    def turns(self):
        result = subprocess.run([str(ROOT / "target/debug/agent-wrangler"), "agents"],
                                env=self.env, text=True, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        return {parts[1]: parts[5] for line in result.stdout.splitlines()
                if len(parts := line.split("\t")) >= 6}

    def assert_excluded(self, panes, labels):
        self.settle()
        for pane in panes:
            text = self.screen(pane)
            self.assertIn("VISIBLE-AGENT", text)
            for label in labels:
                self.assertNotIn(label, text, pane + "\n" + text)
        for label in labels:
            self.assertEqual(self.turns().get(label), "attention", label)

    def test_mixed_views_control_polling_peer_focus_and_shell_cleanup(self):
        modes = ["tree", "sections", "dashboard"]
        peers = [self.start_sidebar(mode) for mode in modes]
        labels = ["HIDDEN-TREE", "HIDDEN-SECTIONS", "HIDDEN-DASHBOARD"]
        ordinary = self.tmux("split-window", "-v", "-t", self.content,
                            "-P", "-F", "#{pane_id}", "bash --noprofile --norc")
        self.tmux("select-pane", "-t", peers[0])
        self.call(ordinary, "VISIBLE-AGENT")
        for pane, label in zip(peers, labels):
            self.call(pane, label)
        self.wait_until(lambda: all("VISIBLE-AGENT" in self.screen(p) for p in peers))
        self.assert_excluded(peers, labels)
        for pane in peers[:2]:
            self.assertIn("tmux-agent-wrangler", self.screen(pane))
        self.assertTrue(self.terminal.screen.contains("VISIBLE-AGENT"))
        self.assertEqual(self.flag(self.content), "")
        self.assertEqual(self.flag(ordinary), "")
        self.assertEqual(self.tmux("list-clients", "-F", "#{client_control_mode}").splitlines().count("1"), 3)
        self.dump("mixed-control")
        self.tmux("select-pane", "-t", peers[0])
        self.tmux("send-keys", "-t", peers[1], "q")
        self.wait_until(lambda: self.flag(peers[1]) == "")
        self.wait_until(lambda: all(labels[1] in self.screen(p) for p in [peers[0], peers[2]]))
        self.launch(peers[1], "sections")
        self.assert_excluded(peers, labels)
        self.dump("control-reclassification")
        for pane in peers:
            self.tmux("select-pane", "-t", pane)
            self.assert_excluded(peers, labels)
        # Move a real peer into another window, preserving its stable pane ID.
        self.tmux("break-pane", "-d", "-s", peers[2], "-n", "REMOTE")
        self.tmux("select-window", "-t", peers[2])
        self.settle()
        self.assertTrue(self.terminal.screen.contains("VISIBLE-AGENT"))
        self.assert_excluded(peers, labels)
        self.dump("peer-across-windows")
        # Lose only the control transports; keep the user terminal attached.
        for client in self.tmux("list-clients", "-F", "#{?client_control_mode,#{client_name},}").splitlines():
            if client.strip():
                self.tmux("detach-client", "-t", client.strip())
        self.wait_until(lambda: self.tmux("list-clients", "-F", "#{client_control_mode}") == "0")
        self.tmux("rename-window", "-t", peers[0], "POLLING")
        self.wait_until(lambda: "POLLING" in self.screen(peers[0]))
        self.assert_excluded(peers, labels)
        self.tmux("select-window", "-t", peers[1])
        self.tmux("select-pane", "-t", peers[0])
        # A normal shell-child exit clears only its flag and restores its agent.
        self.tmux("send-keys", "-t", peers[1], "q")
        self.wait_until(lambda: self.flag(peers[1]) == "")
        self.wait_until(lambda: all(labels[1] in self.screen(p) for p in [peers[0], peers[2]]))
        self.assertEqual(self.flag(peers[0]), "1")
        self.assertEqual(self.flag(peers[2]), "1")
        self.assertIn(peers[1], self.tmux("list-panes", "-s", "-F", "#{pane_id}").splitlines())
        self.dump("polling-shell-cleanup")
        # Restart in the same ID with a different startup-only view option.
        self.launch(peers[1], "tree")
        self.assert_excluded(peers, labels)
        self.dump("polling-reclassification")
        # Only an ordinary content focus may acknowledge the positive control.
        self.tmux("select-pane", "-t", ordinary)
        self.wait_until(lambda: self.turns().get("VISIBLE-AGENT") == "idle")
        for label in [labels[0], labels[2]]:
            self.assertEqual(self.turns().get(label), "attention")
        self.assertEqual(self.terminal.screen.unhandled, 0)

    def test_every_ordered_view_pair_excludes_peer_agents(self):
        modes = ["tree", "sections", "dashboard"]
        peers = [self.start_sidebar("tree"), self.start_sidebar("tree")]
        ordinary = self.tmux("split-window", "-v", "-t", self.content,
                            "-P", "-F", "#{pane_id}", "bash --noprofile --norc")
        self.tmux("select-pane", "-t", self.content)
        self.call(ordinary, "VISIBLE-AGENT")
        labels = ["HIDDEN-FIRST", "HIDDEN-SECOND"]
        for pane, label in zip(peers, labels):
            self.call(pane, label)
        for first in modes:
            for second in modes:
                with self.subTest(first=first, second=second):
                    for pane in peers:
                        self.tmux("send-keys", "-t", pane, "q")
                    self.wait_until(lambda: all(self.flag(p) == "" for p in peers))
                    for pane, mode in zip(peers, [first, second]):
                        self.launch(pane, mode)
                    self.assert_excluded(peers, labels)
                    self.assertEqual(self.turns().get("VISIBLE-AGENT"), "attention")
                    self.dump(first + "-" + second)
        self.assertEqual(self.terminal.screen.unhandled, 0)

    def test_root_quit_is_local_and_physical_peer_company_prevents_auto_close(self):
        peers = [self.start_sidebar(mode, root=True) for mode in ["tree", "sections"]]
        self.tmux("send-keys", "-t", peers[1], "q")
        self.wait_until(lambda: peers[1] not in self.tmux("list-panes", "-F", "#{pane_id}").splitlines())
        self.assertEqual(self.flag(peers[0]), "1")
        peers[1] = self.start_sidebar("dashboard", root=True)
        self.tmux("kill-pane", "-t", self.content)
        for pane in peers:
            self.tmux("select-pane", "-t", pane)
            self.settle()
            self.assertEqual(self.flag(pane), "1")
        self.dump("physical-peers-only")
        self.tmux("send-keys", "-t", peers[1], "q")
        self.wait_until(lambda: peers[1] not in self.tmux("list-panes", "-s", "-F", "#{pane_id}", check=False).splitlines())
        self.wait_until(lambda: not self.tmux("list-panes", "-s", "-F", "#{pane_id}", check=False))


if __name__ == "__main__":
    unittest.main()
