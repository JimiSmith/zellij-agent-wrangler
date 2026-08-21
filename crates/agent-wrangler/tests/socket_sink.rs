//! What a native client sees, against the real daemon.
//!
//! Every other test of the socket sink drives the transport directly. This one
//! runs the program: it registers on the command line, connects to the name that
//! the daemon bound, reports an agent through a hook, and restarts the daemon.
//! Nothing between the two ends is stood in for.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, Stream};

/// How long any wait in this test may take before it is a failure.
const PATIENCE: Duration = Duration::from_secs(20);

/// The user that this run reports as.
///
/// The daemon's socket is named for the user. Without a name of its own, this
/// test would report to the daemon of the client that the developer installed,
/// and assert on that one instead of on the build under test.
fn user() -> String {
    format!("wrangler-socket-test-{}", std::process::id())
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("agent-wrangler-socket-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a state directory");
    dir
}

fn wrangler(dir: &PathBuf) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agent-wrangler"));
    command
        .env("USER", user())
        .env("USERNAME", user())
        .env("XDG_STATE_HOME", dir)
        .env("LOCALAPPDATA", dir)
        // A hook reports nothing at all from a process in no multiplexer, so
        // this run says that it is in one.
        .env("TMUX", "/tmp/tmux-1000/default,1,0")
        .env("TMUX_PANE", "%1")
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_SESSION_NAME")
        .env_remove("ZELLIJ_PANE_ID");
    command
}

fn daemon(dir: &PathBuf) -> Child {
    wrangler(dir)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("a daemon")
}

fn register(dir: &PathBuf, name: &str) {
    let ok = wrangler(dir)
        .args(["register", "socket", name])
        .status()
        .expect("the register command")
        .success();
    assert!(ok, "register said no");
}

/// Reports one agent, as an agent's own lifecycle hook does.
fn report(dir: &PathBuf, session: &str) {
    let mut child = wrangler(dir)
        .args(["hook", "claude", "start"])
        .stdin(Stdio::piped())
        .spawn()
        .expect("the hook command");
    let body = format!(
        r#"{{"session_id":"{session}","cwd":"/home/u/quarry","transcript_path":"/nonexistent/t.jsonl"}}"#
    );
    child
        .stdin
        .take()
        .expect("the hook's stdin")
        .write_all(body.as_bytes())
        .expect("the hook body");
    assert!(child.wait().expect("the hook").success());
}

/// Connects to a socket sink, retrying while the daemon binds it.
///
/// The daemon binds the name while it handles the registration, which is after
/// the client sent it. Every client has this gap to cover.
fn peer(name: &str) -> Stream {
    let until = Instant::now() + PATIENCE;
    loop {
        let ns = name.to_ns_name::<GenericNamespaced>().expect("a name");
        match Stream::connect(ns) {
            Ok(stream) => return stream,
            Err(error) if Instant::now() >= until => panic!("nothing bound {name}: {error}"),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// The next state that this peer receives.
fn state<R: BufRead>(reader: &mut R) -> String {
    let mut line = String::new();
    let read = reader.read_line(&mut line).expect("a state");
    assert!(read > 0, "the daemon closed the connection");
    assert!(
        line.starts_with("wrangler "),
        "not a state message: {line:?}"
    );
    line.trim_end().to_string()
}

/// Reads states until one holds `what`, or the patience runs out.
fn state_holding<R: BufRead>(reader: &mut R, what: &str) -> String {
    let until = Instant::now() + PATIENCE;
    loop {
        let line = state(reader);
        if line.contains(what) {
            return line;
        }
        assert!(Instant::now() < until, "no state ever held {what}");
    }
}

#[test]
fn a_native_client_registers_connects_and_reads_every_state() {
    let dir = scratch();
    let name = format!("agent-wrangler-socket-test-{}.sock", std::process::id());
    let mut running = daemon(&dir);

    register(&dir, &name);
    let one = peer(&name);
    let mut reading = BufReader::new(&one);
    // A client is owed the state the moment it registers, which is before it
    // connects. The daemon holds that state for it.
    state(&mut reading);

    // A second registration for a name that the daemon already publishes to
    // changes nothing. The peer keeps its connection.
    register(&dir, &name);

    // Every peer on one socket receives every state.
    let two = peer(&name);
    let mut also = BufReader::new(&two);
    state(&mut also);

    report(&dir, "socket-one");
    assert!(state_holding(&mut reading, "socket-one").contains("socket-one"));
    assert!(state_holding(&mut also, "socket-one").contains("socket-one"));

    // A peer that disconnects is not a client that has gone. The other peer
    // still receives every state.
    drop(two);
    report(&dir, "socket-two");
    state_holding(&mut reading, "socket-two");

    // A restarted daemon binds again every socket sink that it restored from
    // the state file.
    drop(reading);
    drop(one);
    running.kill().expect("the daemon stops");
    running.wait().expect("the daemon is reaped");
    let mut restarted = daemon(&dir);
    let three = peer(&name);
    let mut again = BufReader::new(&three);
    state(&mut again);

    restarted.kill().expect("the daemon stops");
    restarted.wait().expect("the daemon is reaped");
    let _ = std::fs::remove_dir_all(&dir);
}
