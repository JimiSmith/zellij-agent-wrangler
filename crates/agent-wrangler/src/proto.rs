//! What is said over the daemon's socket, and how one message is told from the
//! next.
//!
//! Every message is one line of JSON ending in a single newline, so a stream is
//! a run of independent lines and a reader that loses its place at one message
//! finds it again at the next. The enums are tagged by a `kind` field, so a
//! decoder dispatches on the tag rather than on the shape.

use std::io::{self, BufRead, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// What an agent's lifecycle hook reports.
///
/// The hook says what it saw and nothing about what it means: the transcript is
/// named rather than read, and the environment is passed on verbatim. Reading is
/// the daemon's, which is what keeps a hook off the critical path of the turn it
/// runs inside.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hook {
    /// Which agent this is, by the name its hooks were installed under.
    pub agent: String,
    /// The event as the agent named it.
    pub event: String,
    pub session_id: String,
    pub cwd: String,
    /// Where the agent is writing the conversation, which is the only place it
    /// says what it has decided to call the session.
    pub transcript: String,
    /// Present only when the hook body carried a genuine JSON boolean.
    pub recoverable: Option<bool>,
    /// The location variables the hook captured, already encoded as one run of
    /// values.
    pub origin: String,
    /// The agent's own process, found by climbing the hook's ancestry.
    pub pid: Option<u32>,
    /// When the hook ran, which is what orders one call for the user against
    /// another.
    pub at: u64,
}

/// Where the daemon delivers a state change.
///
/// One variant per multiplexer, and each says only how to reach that client.
/// Adding a multiplexer is a variant here and an arm in the delivery, with
/// nothing else in the daemon aware of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Sink {
    /// A zellij session, reached by piping into it by name.
    Zellij { session: String },
    /// A named pipe, written to a line at a time.
    Pipe { path: String },
}

/// What is sent to the daemon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Inbound {
    /// An agent reported itself.
    Hook {
        /// The record format the sender speaks. A daemon that speaks another one
        /// is of another build, and stands down rather than answering wrongly.
        format: u32,
        hook: Hook,
    },
    /// A client asked to be delivered to from now on, and to be told the state
    /// as it currently stands.
    Register { format: u32, sink: Sink },
    /// The user reached a session that was calling for them.
    Seen { session: String },
    /// Say the state on this connection and nothing more. What the command line
    /// asks, for looking at what the daemon holds.
    Snapshot,
}

/// What the daemon says back on a connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outbound {
    /// Every session the daemon holds, as a run of records.
    Agents { format: u32, records: String },
}

/// Write one message as one line.
pub fn write_message<W: Write, T: Serialize>(writer: &mut W, message: &T) -> io::Result<()> {
    let mut line = serde_json::to_string(message)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()
}

/// Read one message from a line, or `None` at the end of the stream.
///
/// A line that is not a message this build knows is skipped rather than ending
/// the stream, so one unrecognised message does not cost the connection.
pub fn read_message<R: BufRead, T: DeserializeOwned>(reader: &mut R) -> io::Result<Option<T>> {
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(message) = serde_json::from_str(line) {
            return Ok(Some(message));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook() -> Hook {
        Hook {
            agent: "claude".to_string(),
            event: "needsAttention".to_string(),
            session_id: "9f3c-1a".to_string(),
            cwd: "/home/u/repo".to_string(),
            transcript: "/home/u/.claude/t.jsonl".to_string(),
            recoverable: None,
            origin: "0\u{1f}proto\u{1f}7\u{1f}\u{1f}".to_string(),
            pid: Some(4242),
            at: 1_700_000_000_000,
        }
    }

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(message: T) {
        let mut written = Vec::new();
        write_message(&mut written, &message).unwrap();
        assert_eq!(written.last(), Some(&b'\n'), "one line, newline ended");
        let mut reader = written.as_slice();
        let read: Option<T> = read_message(&mut reader).unwrap();
        assert_eq!(read, Some(message));
    }

    #[test]
    fn every_message_survives_the_round_trip() {
        round_trip(Inbound::Hook {
            format: 3,
            hook: hook(),
        });
        round_trip(Inbound::Register {
            format: 3,
            sink: Sink::Zellij {
                session: "proto".to_string(),
            },
        });
        round_trip(Inbound::Register {
            format: 3,
            sink: Sink::Pipe {
                path: "/tmp/w.pipe".to_string(),
            },
        });
        round_trip(Inbound::Seen {
            session: "9f3c-1a".to_string(),
        });
        round_trip(Inbound::Snapshot);
        round_trip(Outbound::Agents {
            format: 3,
            records: "3\tone\tclaude".to_string(),
        });
    }

    #[test]
    fn a_run_of_messages_reads_back_one_at_a_time() {
        let mut written = Vec::new();
        write_message(&mut written, &Inbound::Snapshot).unwrap();
        write_message(
            &mut written,
            &Inbound::Seen {
                session: "one".to_string(),
            },
        )
        .unwrap();
        let mut reader = written.as_slice();
        assert_eq!(
            read_message::<_, Inbound>(&mut reader).unwrap(),
            Some(Inbound::Snapshot)
        );
        assert_eq!(
            read_message::<_, Inbound>(&mut reader).unwrap(),
            Some(Inbound::Seen {
                session: "one".to_string()
            })
        );
        assert_eq!(read_message::<_, Inbound>(&mut reader).unwrap(), None);
    }

    #[test]
    fn a_line_that_is_not_a_message_costs_the_line_and_not_the_stream() {
        let mut written = Vec::new();
        written.extend_from_slice(b"not json\n");
        written.extend_from_slice(b"{\"kind\":\"from_the_future\"}\n");
        written.extend_from_slice(b"\n");
        write_message(&mut written, &Inbound::Snapshot).unwrap();
        let mut reader = written.as_slice();
        assert_eq!(
            read_message::<_, Inbound>(&mut reader).unwrap(),
            Some(Inbound::Snapshot)
        );
    }

    #[test]
    fn a_message_carrying_a_newline_still_takes_one_line() {
        // A title, a directory or a session name can hold anything at all; the
        // encoding is what keeps one message to one line.
        let awkward = Inbound::Seen {
            session: "one\ntwo".to_string(),
        };
        let mut written = Vec::new();
        write_message(&mut written, &awkward).unwrap();
        assert_eq!(written.iter().filter(|byte| **byte == b'\n').count(), 1);
        let mut reader = written.as_slice();
        assert_eq!(
            read_message::<_, Inbound>(&mut reader).unwrap(),
            Some(awkward)
        );
    }
}
