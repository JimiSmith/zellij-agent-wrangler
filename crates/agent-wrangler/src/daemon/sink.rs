//! This module delivers one state change to one client.
//!
//! A sink says how to reach a client, and says nothing about what the client is.
//! A new multiplexer therefore costs one variant and one arm in this module.
//! Nothing else learns about the new multiplexer.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use agent_wrangler_core::agent::AGENTS_MESSAGE;

use crate::platform::{command, ran, Ran};
use crate::proto::Sink;

/// The time that one client gets to take one delivery.
///
/// A healthy `zellij pipe` returns in twenty to fifty milliseconds, so this
/// limit is forty times what a working client needs. The extra room is worth
/// the cost. A delivery killed too soon never hands its payload over, and that
/// sidebar then holds its last state until something else changes. A machine
/// under load is exactly the moment with something to draw.
///
/// The other end of the choice is the cost of a client that never lets go. This
/// module delivers to clients one after another on one thread, and only after a
/// change. Each wedged client adds this much time to a publish, and nothing
/// adds more.
const PATIENCE: Duration = Duration::from_secs(2);

/// The outcome of one delivery.
///
/// A client that this module cannot reach is a client that went away. This
/// outcome therefore decides whether the daemon keeps the sink. This type is
/// deliberately not an error type. There is nothing to report, and nobody to
/// report it to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    Failed,
    /// The wait ran out, so this module killed the program that handed the
    /// payload over.
    ///
    /// This outcome is not a refusal, and the difference matters. `zellij pipe`
    /// gives the payload to the plugins, and then, now and then, it never
    /// exits. The state arrived. A count of this outcome as an unreachable
    /// client retires a live sidebar that still receives the state. A sidebar
    /// never registers a second time.
    Abandoned,
}

/// Hands one payload to one client.
///
/// Side effect: this function runs a program, or writes to a file. The sink
/// decides which one. Both get no input, and this module discards their output.
/// The exit status is the whole of the answer.
pub fn deliver(sink: &Sink, payload: &str) -> Delivery {
    match sink {
        Sink::Zellij { session } => zellij(session, payload),
        Sink::Pipe { path } => match pipe(Path::new(path), payload) {
            true => Delivery::Sent,
            false => Delivery::Failed,
        },
    }
}

/// Pipes into one named zellij session. The message names no plugin, so every
/// sidebar in that session hears it.
///
/// Side effect: this function runs `zellij`. If `zellij` does not finish within
/// [`PATIENCE`], this function kills it.
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

/// Appends one line to a named pipe. A named pipe reaches a client that is not
/// a process that this module can run.
///
/// This function writes the payload as a single line. The reader at the other
/// end takes one state per read, whatever the order of the writes.
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
        // Both ends of the choice. Below this limit, a machine under load
        // loses its deliveries to the kill. Above this limit, a few wedged
        // clients stop the daemon from any delivery at all, which is the
        // failure that the wait prevents.
        assert!(PATIENCE >= Duration::from_millis(500));
        assert!(PATIENCE <= Duration::from_secs(5));
    }
}
