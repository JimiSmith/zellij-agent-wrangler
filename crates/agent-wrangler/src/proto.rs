//! What the daemon's socket carries, and how one message ends before the next
//! one starts.
//!
//! Every message is one line of JSON with a single newline at the end. A stream
//! is therefore a run of independent lines. A reader that loses its place at
//! one message finds it again at the next line. A `kind` field tags each enum,
//! so a decoder dispatches on the tag and not on the shape.
//!
//! # A variant name and a field name are the bytes
//!
//! No type in this module carries a `#[serde(rename)]`. So serde derives every
//! `kind` value from the variant name, and every JSON key from the field name.
//! Rename either one and the bytes on the wire move. A daemon and a sidebar are
//! installed separately, so the two ends can be different builds, and
//! `read_message` skips a line that it cannot decode without a word. A mismatch
//! therefore shows as a pane that quietly stops updating.
//!
//! Three things depend on these names beyond the live wire. `DeliveryTarget`
//! tags are written to `agents.json`, so a rename there breaks restore on
//! restart. `MonitorEvent` variant names are what a user reads in
//! `agent-wrangler monitor`. `ClientMessage` variant names must match the
//! literals that `agent_wrangler_core::client_message::ClientMessage::encode`
//! writes by hand, and one test below is all that holds the two in step.
//!
//! A type name is free. Nothing serializes it. `MonitorRecord::event` is free
//! for the same reason: `#[serde(flatten)]` lifts the `MonitorEvent` tag and
//! its fields to the top level, so that field name never reaches the wire.

use std::io::{self, BufRead, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use agent_wrangler_core::agent::Process;

/// What an agent's lifecycle hook reports.
///
/// The hook says what it saw and nothing about what it means. It names the
/// transcript but does not read it, and it passes the environment on verbatim.
/// The daemon does the reading. This keeps a hook off the critical path of the
/// turn that it runs inside.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hook {
    /// Which agent this is, by the name of its installed hooks.
    pub agent: String,
    /// The event as the agent named it.
    pub event: String,
    pub session_id: String,
    pub cwd: String,
    /// Where the agent writes the conversation. This file is the only place
    /// where the agent gives its own name for the session.
    pub transcript: String,
    /// Present only when the hook body carried a genuine JSON boolean.
    pub recoverable: Option<bool>,
    /// The child that this hook fired inside, or `None` for a hook that fired
    /// in the session itself. Claude sets it for a subagent and for a teammate
    /// alike, and `session_id` still names the lead.
    pub agent_id: Option<String>,
    /// What kind of child this is. Claude writes the built in type of a
    /// subagent, such as `Explore`, and the name that the lead gave a teammate.
    pub agent_type: Option<String>,
    /// The location variables that the hook captured, already encoded as one
    /// run of values.
    pub origin: String,
    /// The agent's own process. The hook climbs its ancestry to find this
    /// process and dates it there, because only the machine that ran the hook
    /// can date it.
    pub process: Option<Process>,
    /// When the hook ran. This time orders one call for the user against
    /// another.
    pub at: u64,
}

/// Where the daemon delivers a state change.
///
/// There is one variant for each multiplexer, and each variant says only how to
/// reach that client. A new multiplexer needs a variant here and an arm in the
/// delivery. Nothing else in the daemon knows about it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeliveryTarget {
    /// A zellij session. The daemon reaches it with a pipe to its name.
    Zellij { session: String },
    /// A local socket that the daemon binds and listens on. A client that can
    /// hold a connection reads it, and any number of them read one name.
    ///
    /// The field is a name rather than a path. On Windows the socket is a named
    /// pipe reached through a namespace, and the same name serves on both
    /// systems.
    Socket { name: String },
}

/// What the daemon receives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Inbound {
    /// An agent reported itself.
    Hook {
        /// The record format that the sender speaks. A daemon that speaks
        /// another one is of another build. It stands down and does not answer
        /// wrongly.
        format: u32,
        hook: Hook,
    },
    /// A client asked for every delivery from now on, and for the state as it
    /// stands now.
    Register {
        format: u32,
        sink: DeliveryTarget,
        /// The words that this client runs to announce a call for the user.
        /// Empty for a client that wants no announcement.
        ///
        /// A client names the announcement but does not make it. Every client
        /// holds the same state. If each client announced the state itself, one
        /// call reaches the user once for each client.
        #[serde(default)]
        notify: Vec<String>,
    },
    /// The user reached a session that called for them.
    Seen { session: String },
    /// A request for the state on this connection and nothing more. The command
    /// line sends this message to show what the daemon holds.
    Snapshot,
    /// A request for everything from now on, on this connection, until the
    /// connection is dropped.
    Monitor { format: u32 },
}

/// What a client says on the transport that already carries its state.
///
/// This is deliberately narrower than [`Inbound`]. The lines arrive on a pipe
/// that a plugin writes to. A plugin must not report an agent or register a
/// client. A line that says one of those things does not decode here at all,
/// so no arm has to turn it down.
///
/// A variant that appears in both types is written the same way. The daemon's
/// own socket and a client transport carry the same words. A test below fails
/// if the two drift apart. That test covers the `Seen` line only.
///
/// A beat appears in this type alone. A beat says that the client on its own
/// transport can still send a message. The daemon's own socket is a transport
/// to no client, so there is nothing there for a beat to say anything about.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientMessage {
    /// The user reached a session that called for them.
    Seen { session: String },
    /// The client is there and can still send a message. This says nothing
    /// else.
    Beat,
}

/// One message that the daemon received or sent, and the time of it.
///
/// These records answer one question. The state reaches a sidebar, the sidebar
/// answers on the same transport, and that answer arrives back here as another
/// message. A run of these records says how fast that circle turns, and what
/// starts it again.
///
/// These records hold messages only. What the daemon decided, polled or noticed
/// in between is its own business and is not a record here. A watcher of that
/// reads the daemon's diary rather than its post.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorRecord {
    /// Milliseconds since the epoch, read where the message was.
    pub at: u64,
    #[serde(flatten)]
    pub event: MonitorEvent,
}

/// A message, and the direction it went in.
///
/// On a message that arrives, `told` says whether the message changed anything.
/// The change, and not the arrival, is what owes the clients a delivery. A run
/// of messages that each told the daemon nothing is a loop that turns over but
/// does not move.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MonitorEvent {
    /// In: an agent's hook reported.
    Hook {
        agent: String,
        event: String,
        session: String,
        told: bool,
    },
    /// In: a client asked for every delivery from now on.
    Registered { sink: DeliveryTarget },
    /// In: a client said that the user reached a session that called for them.
    Seen { session: String, told: bool },
    /// In: a client said that it is there and can still send a message.
    ///
    /// The daemon gives up on a client that stops saying this. A run of these
    /// says how fast the circle turns while nothing else happens. A client with
    /// something to report sends no separate beat, so a `Seen` counts as one of
    /// these and no record here follows it.
    Beat { sink: DeliveryTarget },
    /// In: something asked for the state on its own connection.
    Asked,
    /// Out: the state goes to this client, with this many agents in it.
    ///
    /// One of these is written for each state that goes out. A delivery is a
    /// write and not a process run. There is nothing to say afterwards about a
    /// delivery that landed, and no time to measure.
    Delivering { sink: DeliveryTarget, agents: usize },
    /// Out: a delivery to this client did not land.
    ///
    /// Only a failure is worth a second record. The daemon learns of it after
    /// the state went out, because nothing waits on a write. No count of these
    /// retires a client. A client leaves for going quiet and for nothing else.
    Failed { sink: DeliveryTarget },
    /// Out: the daemon gave up on this client and delivers to it no more.
    ///
    /// A client of either kind leaves for one reason. It said nothing for
    /// longer than the daemon waits. Without this record, a feed that stopped
    /// has no explanation anywhere.
    Retired { sink: DeliveryTarget },
    /// Records that the daemon cannot hand over fast enough. A watcher that
    /// falls behind loses records and does not hold the daemon up. The daemon
    /// tells the watcher how many records it lost, so the watcher does not
    /// believe that it saw everything.
    Missed { records: u64 },
}

/// What the daemon says back on a connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outbound {
    /// Every session that the daemon holds, as a run of records.
    Agents { format: u32, records: String },
}

/// Write one message as one line.
pub fn write_message<W: Write, T: Serialize>(writer: &mut W, message: &T) -> io::Result<()> {
    let mut line = serde_json::to_string(message)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()
}

/// The largest size that one message can be.
///
/// A reader with no limit grows to the size of its input. A sender that never
/// writes a newline can therefore take the daemon down, and anything that
/// connects can do it. This limit is far above any real message.
///
/// The largest message is a run of records. A record carries two transcript
/// records for a client to draw a preview from, so it runs a few kilobytes. It
/// runs to tens of kilobytes for an agent that is part way through a write. Ten
/// such agents at once come to a few hundred kilobytes. That is a spike rather
/// than a steady state, and it is well under this limit.
const LONGEST: u64 = 4 * 1024 * 1024;

/// Read one message from a line, or `None` at the end of the stream.
///
/// The reader skips a line that this build does not know as a message, and the
/// stream continues. One unrecognized message therefore does not cost the
/// connection. A line longer than [`LONGEST`] ends the stream, because no real
/// message is that long. A reader that continues past the limit reads the rest
/// of that line as messages.
pub fn read_message<R: BufRead, T: DeserializeOwned>(reader: &mut R) -> io::Result<Option<T>> {
    loop {
        let mut line = String::new();
        let mut bounded = io::Read::take(&mut *reader, LONGEST);
        if bounded.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        if line.len() as u64 >= LONGEST && !line.ends_with('\n') {
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
            agent_id: None,
            agent_type: None,
            origin: "0\u{1f}proto\u{1f}7\u{1f}\u{1f}".to_string(),
            process: Some(Process {
                pid: 4242,
                started: Some(agent_wrangler_core::agent::ProcessStartStamp(918_273)),
            }),
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
            sink: DeliveryTarget::Zellij {
                session: "proto".to_string(),
            },
            notify: vec!["notify-send".to_string(), "--urgency".to_string()],
        });
        round_trip(Inbound::Register {
            format: 3,
            sink: DeliveryTarget::Socket {
                name: "agent-wrangler-tmux-work.sock".to_string(),
            },
            notify: Vec::new(),
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
    fn a_client_the_daemon_gave_up_on_says_so() {
        round_trip(MonitorRecord {
            at: 1_700_000_000_000,
            event: MonitorEvent::Retired {
                sink: DeliveryTarget::Socket {
                    name: "agent-wrangler-tmux-work.sock".to_string(),
                },
            },
        });
    }

    #[test]
    fn a_delivery_that_failed_says_which_client_refused_it() {
        round_trip(MonitorRecord {
            at: 1_700_000_000_000,
            event: MonitorEvent::Failed {
                sink: DeliveryTarget::Zellij {
                    session: "proto".to_string(),
                },
            },
        });
    }

    #[test]
    fn what_a_client_says_on_its_own_transport_is_what_the_daemon_receives() {
        // Both readers must accept the same line. A client writes that line
        // with no JSON writer at all, so nothing but this test keeps the two
        // ends in step.
        let line = agent_wrangler_core::client_message::ClientMessage::Seen(
            agent_wrangler_core::agent::SessionId::new("9f3c-1a").unwrap(),
        )
        .encode();
        let session = "9f3c-1a".to_string();
        assert_eq!(
            read_message::<_, ClientMessage>(&mut line.as_bytes()).unwrap(),
            Some(ClientMessage::Seen {
                session: session.clone()
            })
        );
        assert_eq!(
            read_message::<_, Inbound>(&mut line.as_bytes()).unwrap(),
            Some(Inbound::Seen { session })
        );
    }

    #[test]
    fn a_beat_reaches_a_client_transport_and_no_other() {
        // A beat is about the transport that carried it. The daemon's own
        // socket is a transport to no client. A beat written there costs the
        // line and not the connection, because a line that does not decode is
        // passed over.
        let line = agent_wrangler_core::client_message::ClientMessage::Beat.encode();
        assert_eq!(
            read_message::<_, ClientMessage>(&mut line.as_bytes()).unwrap(),
            Some(ClientMessage::Beat)
        );
        assert_eq!(
            read_message::<_, Inbound>(&mut line.as_bytes()).unwrap(),
            None
        );
    }

    #[test]
    fn nothing_but_a_client_message_decodes_on_a_client_transport() {
        // A plugin writes on this pipe. A plugin that reported an agent, or
        // registered a client, is believed. Nothing turns those down at run
        // time, because they do not decode at all.
        let mut written = Vec::new();
        write_message(&mut written, &Inbound::Snapshot).unwrap();
        write_message(
            &mut written,
            &Inbound::Register {
                format: 3,
                sink: DeliveryTarget::Zellij {
                    session: "proto".to_string(),
                },
                notify: Vec::new(),
            },
        )
        .unwrap();
        write_message(
            &mut written,
            &Inbound::Seen {
                session: "one".to_string(),
            },
        )
        .unwrap();
        let mut reader = written.as_slice();
        assert_eq!(
            read_message::<_, ClientMessage>(&mut reader).unwrap(),
            Some(ClientMessage::Seen {
                session: "one".to_string()
            }),
            "the two before it were passed over"
        );
    }

    #[test]
    fn a_client_that_names_no_notifier_still_registers() {
        // The field is what a client says about the announcement of calls. A
        // message written without the field is a client that wants no
        // announcement, and not a message that cannot be read.
        let line = r#"{"kind":"register","format":3,"sink":{"kind":"zellij","session":"proto"}}"#;
        let mut reader = line.as_bytes();
        assert_eq!(
            read_message::<_, Inbound>(&mut reader).unwrap(),
            Some(Inbound::Register {
                format: 3,
                sink: DeliveryTarget::Zellij {
                    session: "proto".to_string()
                },
                notify: Vec::new(),
            })
        );
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
    fn a_line_that_never_ends_ends_the_stream() {
        // Without the limit, anything that can connect writes forever and takes
        // the daemon down with it.
        let endless = vec![b'x'; (LONGEST + 16) as usize];
        let mut reader = endless.as_slice();
        assert_eq!(read_message::<_, Inbound>(&mut reader).unwrap(), None);
    }

    #[test]
    fn a_message_carrying_a_newline_still_takes_one_line() {
        // A title, a directory or a session name can hold anything at all. The
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
