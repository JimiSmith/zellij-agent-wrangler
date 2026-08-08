//! Delivering one state change to one client.
//!
//! A sink says how to reach a client and nothing about what the client is. That
//! is what keeps adding a multiplexer to a variant and an arm here, with nothing
//! else having to learn about it.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use agent_wrangler_core::agent::AGENTS_MESSAGE;

use crate::proto::Sink;

/// Whether the delivery got through.
///
/// A client that cannot be reached is one that has gone, so the answer is what
/// decides whether the sink is kept. It is deliberately not an error type: there
/// is nothing to report and nobody to report it to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    Failed,
}

impl Delivery {
    pub fn sent(self) -> bool {
        matches!(self, Delivery::Sent)
    }
}

/// Hand one payload to one client.
///
/// Side effect: runs a program, or writes to a file, depending on the sink. Both
/// are given no input and their output is discarded; the exit status is the
/// whole of what is read back.
pub fn deliver(sink: &Sink, payload: &str) -> Delivery {
    let ok = match sink {
        Sink::Zellij { session } => zellij(session, payload),
        Sink::Pipe { path } => pipe(Path::new(path), payload),
    };
    match ok {
        true => Delivery::Sent,
        false => Delivery::Failed,
    }
}

/// Pipe into one named zellij session, addressed to no plugin so that every
/// sidebar in that session hears it.
fn zellij(session: &str, payload: &str) -> bool {
    Command::new("zellij")
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
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
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
}
