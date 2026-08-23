//! What a client tells the daemon, as the one line that carries it.
//!
//! This module writes a message and never reads one back. A sidebar that runs
//! as wasm takes this crate without a JSON reader, so the line here is built
//! rather than serialized.

use std::time::Duration;

use crate::agent::SessionId;

/// How often a client tells the daemon that it is there.
///
/// Any line from a client is a beat. A client with something to say sends no
/// separate one. This is the longest time that a client stays quiet. The
/// daemon waits longer than this before it gives up on a client. A test in the
/// daemon holds the two numbers apart.
pub const BEAT: Duration = Duration::from_secs(30);

/// One thing that a client tells the daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Told {
    /// The user reached a session that called for them.
    Seen(SessionId),
    /// The client is there. This says nothing else.
    Beat,
}

impl Told {
    /// This message as the one line that carries it.
    ///
    /// A [`SessionId`] goes into the line as it stands, because that type
    /// admits no character that JSON must escape.
    pub fn encode(&self) -> String {
        match self {
            Told::Seen(session) => {
                format!(r#"{{"kind":"seen","session":"{}"}}"#, session.as_str())
            }
            Told::Beat => r#"{"kind":"beat"}"#.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answered_call_names_its_session() {
        let told = Told::Seen(SessionId::new("9f3c-1a").unwrap());
        assert_eq!(told.encode(), r#"{"kind":"seen","session":"9f3c-1a"}"#);
    }

    #[test]
    fn a_beat_says_only_that_the_client_is_there() {
        assert_eq!(Told::Beat.encode(), r#"{"kind":"beat"}"#);
    }

    #[test]
    fn nothing_a_session_can_be_called_breaks_the_line_it_travels_on() {
        let awkward = SessionId::new("one\"two\nthree\\four").unwrap();
        let line = Told::Seen(awkward).encode();
        assert_eq!(line, r#"{"kind":"seen","session":"one_two_three_four"}"#);
        assert!(!line.contains('\n'), "one line");
    }
}
