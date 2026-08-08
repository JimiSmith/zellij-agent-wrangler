//! One agent session: who it is, what it calls itself, where it reported itself
//! from, and whose turn it is.
//!
//! A record is written as one line of delimited text and read back the same
//! way, so the two ends of the wire can be built and updated separately. Every
//! field is stripped of the characters that would split a line, which is why the
//! fields are built rather than assigned.

use crate::origin::Origin;

/// The character between a record's fields, and the one between records. Both
/// are excluded from every field, so a run of records splits exactly.
pub(crate) const FIELD: char = '\t';
pub(crate) const RECORD: char = '\n';

/// The shape of a record, which every one of them leads with.
///
/// What writes a record and what reads it are installed separately and updated
/// separately, so one of them can be older than the other. Saying which shape a
/// record is written in is what turns that into something the reader can report
/// rather than a run of records it silently makes nothing of.
pub const FORMAT: u32 = 3;

/// The message every record travels in.
///
/// One message carries the whole set rather than one session's news, because
/// what a reader needs is the state, not the events that led to it: a reader
/// that misses a message is corrected by the next one instead of being left a
/// record behind.
pub const AGENTS_MESSAGE: &str = "wrangler:agents";

/// The line every state message leads with, naming what it is and what shape
/// its records are in.
const HEADER: &str = "wrangler";

/// A run of records as a whole statement of what there is.
///
/// The header is what makes "there are no agents" a thing that can be *said*.
/// Without it an empty state and an empty message are the same bytes, and a
/// message that arrived truncated, or one sent by something else entirely,
/// would read as an instruction to forget every agent there is.
pub fn state(records: &str) -> String {
    format!("{HEADER} {FORMAT}{RECORD}{records}")
}

/// The format and the records of a state message, or `None` for anything that
/// is not one.
pub fn read_state(payload: &str) -> Option<(u32, &str)> {
    let (head, records) = payload.split_once(RECORD)?;
    let (name, format) = head.split_once(' ')?;
    match name == HEADER {
        true => Some((format.parse().ok()?, records)),
        false => None,
    }
}

/// The id an agent gives its own session, which is what that session is filed
/// under.
///
/// The id travels inside delimited text, so the only constructor replaces every
/// character that is not a letter, a digit, `.`, `_` or `-`, and refuses an id
/// that has no such character to begin with. A `SessionId` that could split a
/// field or a record cannot be built.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(text: &str) -> Option<Self> {
        if text.is_empty() {
            return None;
        }
        Some(SessionId(
            text.chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                        c
                    } else {
                        '_'
                    }
                })
                .collect(),
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whose turn it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Turn {
    /// The agent is waiting, and wants nothing.
    #[default]
    Idle,
    /// The agent is mid-turn.
    Working,
    /// The agent wants the user.
    Attention,
}

impl Turn {
    fn encode(self) -> &'static str {
        match self {
            Turn::Idle => "idle",
            Turn::Working => "working",
            Turn::Attention => "attention",
        }
    }

    fn decode(text: &str) -> Option<Self> {
        match text {
            "idle" => Some(Turn::Idle),
            "working" => Some(Turn::Working),
            "attention" => Some(Turn::Attention),
            _ => None,
        }
    }
}

/// What a session is called by, as it was found. Every field is empty until
/// something says otherwise: an agent that has not titled itself yet, and one
/// that never will, look the same from here.
///
/// These are the facts a label is *composed from* rather than the label itself,
/// so what draws a session can be told to spell it differently without the
/// agents having to report themselves again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    /// The name of the directory the agent is working in.
    pub dir: String,
    /// The agent's own name when it is a teammate of another session, which is
    /// what tells the two apart.
    pub name: String,
    /// The color the agent shows for this session, by the agent's own name for
    /// it. Empty for a session with none, and for an agent that assigns none.
    ///
    /// The name is the fact; which of the terminal's colors draws it is not
    /// settled here.
    pub color: String,
    /// The title the session gave itself.
    pub title: String,
}

/// What a line turned out to be.
///
/// A record written in another format is told from a line that is not a record
/// at all, because the two mean different things: the first says the two ends of
/// the wire are out of step with each other, and the second says nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    Known(Agent),
    Foreign(u32),
    None,
}

/// One agent session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub session: SessionId,
    pub agent: String,
    pub meta: Meta,
    /// Where the agent's own hook was invoked, as its environment described it.
    pub origin: Origin,
    /// The agent's own process, as its hook found it by climbing its ancestry.
    /// `None` for a hook that could not say, which is a record nothing can check
    /// the liveness of.
    pub pid: Option<u32>,
    pub turn: Turn,
    /// When the agent last called for the user, as the clock read at the time.
    /// It is taken once, where the call happens, so everything downstream orders
    /// the calls the same way without comparing clocks of its own.
    pub raised: u64,
}

/// Replace every character that would split a record or a field, and every
/// control character, with a space.
fn field(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

impl Agent {
    pub fn new(session: SessionId, agent: &str, meta: Meta, origin: Origin) -> Self {
        Agent {
            session,
            agent: field(agent),
            meta: Meta {
                dir: field(&meta.dir),
                name: field(&meta.name),
                color: field(&meta.color),
                title: field(&meta.title),
            },
            origin,
            pid: None,
            turn: Turn::default(),
            raised: 0,
        }
    }

    /// The record as one line: the format, then everything about the session,
    /// with the title last because it is the one field allowed to hold anything
    /// at all.
    pub fn encode(&self) -> String {
        let pid = self.pid.map(|id| id.to_string()).unwrap_or_default();
        format!(
            "{FORMAT}{FIELD}{}{FIELD}{}{FIELD}{pid}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}",
            self.session.as_str(),
            self.agent,
            self.turn.encode(),
            self.raised,
            self.meta.dir,
            self.meta.name,
            self.meta.color,
            self.origin.encode(),
            self.meta.title,
        )
    }

    /// What a line turned out to be.
    ///
    /// The title is the whole remainder of the line, so a title carrying the
    /// field character would still parse; it cannot, because the constructor
    /// takes that character out.
    pub fn decode(line: &str) -> Record {
        let mut fields = line.splitn(11, FIELD);
        match fields.next().and_then(|format| format.parse::<u32>().ok()) {
            Some(FORMAT) => {}
            Some(other) => return Record::Foreign(other),
            None => return Record::None,
        }
        match Agent::read(fields) {
            Some(agent) => Record::Known(agent),
            None => Record::None,
        }
    }

    /// A record's fields, after the one saying which format they are in.
    fn read<'a>(mut fields: impl Iterator<Item = &'a str>) -> Option<Self> {
        let session = SessionId::new(fields.next()?)?;
        let agent = fields.next()?;
        let pid = fields.next()?;
        let turn = Turn::decode(fields.next()?)?;
        let raised = fields.next()?.parse().ok()?;
        let dir = fields.next()?.to_string();
        let name = fields.next()?.to_string();
        let color = fields.next()?.to_string();
        let origin = Origin::decode(fields.next()?);
        let title = fields.next()?.to_string();
        let pid = match pid.is_empty() {
            true => None,
            false => Some(pid.parse().ok()?),
        };
        Some(Agent {
            pid,
            turn,
            raised,
            ..Agent::new(
                session,
                agent,
                Meta {
                    dir,
                    name,
                    color,
                    title,
                },
                origin,
            )
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::origin::Origin;

    pub(crate) fn session(text: &str) -> SessionId {
        SessionId::new(text).unwrap()
    }

    pub(crate) fn meta(dir: &str, name: &str, title: &str) -> Meta {
        Meta {
            dir: dir.to_string(),
            name: name.to_string(),
            color: String::new(),
            title: title.to_string(),
        }
    }

    /// An origin describing a zellij pane, which is what a hook invoked in one
    /// captures.
    pub(crate) fn at_pane(pane: u32) -> Origin {
        Origin::from(|name| match name {
            "ZELLIJ" => Some("0".to_string()),
            "ZELLIJ_SESSION_NAME" => Some("wrangler-proto".to_string()),
            "ZELLIJ_PANE_ID" => Some(pane.to_string()),
            _ => None,
        })
    }

    pub(crate) fn agent(id: &str, pane: u32) -> Agent {
        Agent::new(
            session(id),
            "claude",
            meta("wrangler", "", ""),
            at_pane(pane),
        )
    }

    pub(crate) fn nowhere(id: &str) -> Agent {
        Agent::new(
            session(id),
            "claude",
            meta("wrangler", "", ""),
            Origin::default(),
        )
    }

    pub(crate) fn colored(id: &str, color: &str) -> Agent {
        Agent::new(
            session(id),
            "claude",
            Meta {
                color: color.to_string(),
                ..meta("wrangler", "", "")
            },
            at_pane(1),
        )
    }

    /// The record an agent's hooks send when the turn changes: everything the
    /// session is, plus whose turn it now is.
    pub(crate) fn reporting(id: &str, pane: u32, turn: Turn, raised: u64) -> Agent {
        Agent {
            turn,
            raised,
            ..agent(id, pane)
        }
    }

    #[test]
    fn a_session_id_keeps_nothing_that_could_split_a_field() {
        let id = SessionId::new("a/b\tc,d=e\nf").unwrap();
        assert_eq!(id.as_str(), "a_b_c_d_e_f");
    }

    #[test]
    fn a_session_with_no_id_at_all_is_no_session() {
        assert_eq!(SessionId::new(""), None);
    }

    #[test]
    fn a_record_survives_the_round_trip() {
        for record in [agent("one", 3), nowhere("two")] {
            assert_eq!(Agent::decode(&record.encode()), Record::Known(record));
        }
    }

    #[test]
    fn a_pid_survives_the_round_trip_and_so_does_having_none() {
        let with = Agent {
            pid: Some(4242),
            ..agent("one", 3)
        };
        assert_eq!(Agent::decode(&with.encode()), Record::Known(with));
        let without = agent("two", 3);
        assert_eq!(without.pid, None);
        assert_eq!(Agent::decode(&without.encode()), Record::Known(without));
    }

    #[test]
    fn an_origin_survives_the_round_trip_inside_a_record() {
        let record = agent("one", 7);
        let Record::Known(read) = Agent::decode(&record.encode()) else {
            panic!("not a record");
        };
        assert_eq!(read.origin.get("ZELLIJ_PANE_ID"), Some("7"));
        assert_eq!(
            read.origin.get("ZELLIJ_SESSION_NAME"),
            Some("wrangler-proto")
        );
    }

    #[test]
    fn a_title_cannot_split_the_record_it_sits_at_the_end_of() {
        let record = Agent::new(
            session("one"),
            "claude",
            meta("d", "", "a\tb\nc"),
            at_pane(1),
        );
        assert_eq!(record.meta.title, "a b c");
        assert_eq!(Agent::decode(&record.encode()), Record::Known(record));
    }

    #[test]
    fn a_line_that_is_not_a_record_decodes_to_nothing() {
        for line in [
            "",
            "one",
            "3\tone\tclaude",
            "3\tone\tclaude\t\tidle\t0\tdir",
            "3\tone\tclaude\tx\tidle\t0\tdir\t\t\t\ttitle",
        ] {
            assert_eq!(Agent::decode(line), Record::None, "{line}");
        }
    }

    #[test]
    fn a_turn_survives_the_round_trip() {
        for turn in [Turn::Idle, Turn::Working, Turn::Attention] {
            let record = reporting("one", 3, turn, 7);
            assert_eq!(Agent::decode(&record.encode()), Record::Known(record));
        }
    }

    #[test]
    fn a_record_with_no_turn_it_recognises_decodes_to_nothing() {
        assert_eq!(
            Agent::decode("3\tone\tclaude\t\tdozing\t0\tdir\t\t\t\t"),
            Record::None
        );
    }

    #[test]
    fn a_state_message_survives_the_round_trip() {
        let records = format!("{}\n{}", agent("one", 3).encode(), agent("two", 4).encode());
        let message = state(&records);
        assert_eq!(read_state(&message), Some((FORMAT, records.as_str())));
    }

    #[test]
    fn having_no_agents_is_something_that_can_be_said() {
        // The difference this whole header exists for: a state with nothing in
        // it is a message, where nothing at all is not.
        let empty = state("");
        assert_eq!(read_state(&empty), Some((FORMAT, "")));
        assert_eq!(read_state(""), None);
    }

    #[test]
    fn anything_that_is_not_a_state_message_is_not_read_as_one() {
        for text in [
            "",
            "wrangler",
            "wrangler 3",
            "3\tone\tclaude",
            "somethingelse 3\n",
        ] {
            assert_eq!(read_state(text), None, "{text}");
        }
    }

    #[test]
    fn a_state_message_says_which_format_it_is_in() {
        let older = "wrangler 1\n3\tone\tclaude";
        assert_eq!(read_state(older), Some((1, "3\tone\tclaude")));
    }

    #[test]
    fn a_color_survives_the_round_trip() {
        let record = colored("one", "purple");
        assert_eq!(Agent::decode(&record.encode()), Record::Known(record));
    }
}
