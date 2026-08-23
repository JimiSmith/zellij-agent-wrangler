//! What a client tells the daemon, as the one line that carries it.
//!
//! This module writes a message and never reads one back. A sidebar that runs
//! as wasm takes this crate without a JSON reader, so the line here is built
//! rather than serialized.

use std::time::Duration;

use crate::agent::SessionId;

/// How often a client tells the daemon that it is there.
///
/// Any line from a client is a beat, so a client with something to say sends no
/// separate one. This is therefore the longest that a working client stays
/// quiet. The daemon gives up on a client that stayed quiet for longer than
/// `SILENCE`, and a test in the daemon holds the two numbers apart.
///
/// A zellij sidebar does not choose this time. It writes only while it handles
/// a message, so the daemon's own beat sets its cadence. Both numbers are the
/// same, so one client is not quieter than the other.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// One thing that a client tells the daemon.
///
/// A variant name here is load bearing. `encode` writes the `kind` value as a
/// literal, and the daemon reads it with a serde tag derived from the variant
/// name of its own `ClientMessage`. Rename a variant on one side alone and the
/// line stops decoding, with nothing to say why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientMessage {
    /// The user reached a session that called for them.
    Seen(SessionId),
    /// The client is there and can still send a message. This says nothing
    /// else.
    Beat,
}

impl ClientMessage {
    /// This message as the one line that carries it.
    ///
    /// A [`SessionId`] goes into the line as it stands, because that type
    /// admits no character that JSON must escape.
    pub fn encode(&self) -> String {
        match self {
            ClientMessage::Seen(session) => {
                format!(r#"{{"kind":"seen","session":"{}"}}"#, session.as_str())
            }
            ClientMessage::Beat => r#"{"kind":"beat"}"#.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answered_call_names_its_session() {
        let told = ClientMessage::Seen(SessionId::new("9f3c-1a").unwrap());
        assert_eq!(told.encode(), r#"{"kind":"seen","session":"9f3c-1a"}"#);
    }

    #[test]
    fn a_beat_says_only_that_the_client_is_there() {
        assert_eq!(ClientMessage::Beat.encode(), r#"{"kind":"beat"}"#);
    }

    #[test]
    fn nothing_a_session_can_be_called_breaks_the_line_it_travels_on() {
        let awkward = SessionId::new("one\"two\nthree\\four").unwrap();
        let line = ClientMessage::Seen(awkward).encode();
        assert_eq!(line, r#"{"kind":"seen","session":"one_two_three_four"}"#);
        assert!(!line.contains('\n'), "one line");
    }
}
