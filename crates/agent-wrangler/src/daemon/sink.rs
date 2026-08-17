//! Delivering one state change to one client.
//!
//! A sink says how to reach a client and nothing about what the client is. That
//! is what keeps adding a multiplexer to a variant and an arm here, with nothing
//! else having to learn about it.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use agent_wrangler_core::agent::AGENTS_MESSAGE;

use crate::platform::{command, ran, Ran};
use crate::proto::Sink;

/// How long one client is given to take one delivery.
///
/// A healthy `zellij pipe` is back in twenty to fifty milliseconds, so this is
/// forty times over what a working client needs. The room is worth having:
/// killed too soon, the one delivery that had not yet handed its payload over
/// leaves that sidebar drawing its last state until something else changes, and
/// a machine under load is exactly when there is something to draw.
///
/// The other end of the choice is what a client that never lets go costs.
/// Clients are delivered to one after another on one thread, and only when
/// something changed, so each wedged one adds this much to a publish and
/// nothing adds more.
const PATIENCE: Duration = Duration::from_secs(2);

/// What became of one delivery.
///
/// A client that cannot be reached is one that has gone, so the answer is what
/// decides whether the sink is kept. It is deliberately not an error type: there
/// is nothing to report and nobody to report it to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    Failed,
    /// The wait ran out, so the program handing the payload over was killed.
    ///
    /// Not a refusal, and the difference matters: `zellij pipe` gives the
    /// payload to the plugins and then, now and then, never exits. The state is
    /// there. Counting that as a client that could not be reached would retire
    /// a sidebar that is alive and being drawn to, and a sidebar never
    /// registers a second time.
    Abandoned,
}

/// Hand one payload to one client.
///
/// Side effect: runs a program, or writes to a file, depending on the sink. Both
/// are given no input and their output is discarded; the exit status is the
/// whole of what is read back.
pub fn deliver(sink: &Sink, payload: &str) -> Delivery {
    match sink {
        Sink::Zellij { session } => zellij(session, payload),
        Sink::Pipe { path } => match pipe(Path::new(path), payload) {
            true => Delivery::Sent,
            false => Delivery::Failed,
        },
    }
}

/// Pipe into one named zellij session, addressed to no plugin so that every
/// sidebar in that session hears it.
///
/// Side effect: runs `zellij`, and kills it if it has not finished within
/// [`PATIENCE`].
fn zellij(session: &str, payload: &str) -> Delivery {
    let mut piping = command("zellij");
    piping
        .args([
            "--session",
            session,
            "pipe",
            "--name",
            AGENTS_MESSAGE,
            "--",
            payload,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match ran(&mut piping, PATIENCE) {
        Ran::Worked => Delivery::Sent,
        Ran::Failed => Delivery::Failed,
        Ran::Abandoned => Delivery::Abandoned,
    }
}

/// Append one line to a named pipe, which is how a client that is not a process
/// this can run is reached.
///
/// The payload is written as a single line, so the reader at the other end takes
/// one state per read regardless of how the writes interleave.
fn pipe(path: &Path, payload: &str) -> bool {
    let one_line: String = payload
        .chars()
        .map(|c| if c == '\n' { '\u{1e}' } else { c })
        .collect();
    OpenOptions::new()
        .append(true)
        .open(path)
        .and_then(|mut file| writeln!(file, "{one_line}"))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pipe_that_is_not_there_is_a_client_that_has_gone() {
        let sink = Sink::Pipe {
            path: "/nonexistent/agent-wrangler/pipe".to_string(),
        };
        assert_eq!(deliver(&sink, "anything"), Delivery::Failed);
    }

    #[test]
    fn a_run_of_records_reaches_a_pipe_as_one_line() {
        let dir = std::env::temp_dir().join("agent-wrangler-sink-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("one.pipe");
        std::fs::write(&path, "").unwrap();
        let sink = Sink::Pipe {
            path: path.to_string_lossy().to_string(),
        };
        assert_eq!(deliver(&sink, "first\nsecond"), Delivery::Sent);
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written.lines().count(), 1);
        assert!(written.contains('\u{1e}'), "the record break is kept");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_wedged_client_costs_a_bounded_part_of_every_delivery() {
        // Both ends of the choice. Short of this, a machine under load has its
        // deliveries killed on the way out; beyond it, a handful of wedged
        // clients is a daemon that has stopped saying anything, which is the
        // failure the wait was written for.
        assert!(PATIENCE >= Duration::from_millis(500));
        assert!(PATIENCE <= Duration::from_secs(5));
    }
}
