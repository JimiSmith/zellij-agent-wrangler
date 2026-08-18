//! One agent session: who it is, what it calls itself, where it reported itself
//! from, and whose turn it is.
//!
//! A record is written as one line of delimited text and read back the same way.
//! The two ends of the wire can therefore be built and updated separately. Every
//! field loses the characters that can split a line, which is why the code
//! builds the fields rather than assigns them.

use crate::origin::Origin;

/// The character between a record's fields, and the one between records. Both
/// are excluded from every field, so a run of records splits exactly.
pub(crate) const FIELD: char = '\t';
pub(crate) const RECORD: char = '\n';

/// The shape of a record, which every record leads with.
///
/// The writer of a record and the reader of a record are installed separately
/// and updated separately. One of them can therefore be older than the other. A
/// record that names its own shape lets the reader report the difference.
/// Without the shape, the reader makes nothing of the records and says nothing.
pub const FORMAT: u32 = 4;

/// The message that every record travels in.
///
/// One message carries the whole set rather than the news of one session. A
/// reader needs the state, not the events that led to it. The next message
/// corrects a reader that missed a message, so the reader never falls a record
/// behind.
pub const AGENTS_MESSAGE: &str = "wrangler:agents";

/// The line that every state message leads with. The line names the message and
/// the shape of its records.
const HEADER: &str = "wrangler";

/// A run of records as a whole statement of what there is.
///
/// The header is what makes "there are no agents" something that a message can
/// say. Without the header, an empty state and an empty message are the same
/// bytes. A truncated message, or a message from something else, then reads as
/// an instruction to forget every agent.
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

/// The id that an agent gives its own session. A session is stored under this
/// id.
///
/// The id travels inside delimited text. The only constructor therefore replaces
/// every character that is not a letter, a digit, `.`, `_` or `-`. The
/// constructor refuses an id with no characters at all. Nothing can build a
/// `SessionId` that splits a field or a record.
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
/// something says otherwise. An agent with no title yet, and an agent that never
/// takes one, look the same from here.
///
/// These are the facts that a label is composed from, not the label itself. The
/// code that draws a session can therefore spell the label differently, and the
/// agents do not report themselves again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    /// The name of the directory that the agent works in.
    pub dir: String,
    /// The agent's own name when it is a teammate of another session. The name
    /// is what tells the two apart.
    pub name: String,
    /// The color that the agent shows for this session, under the agent's own
    /// name for that color. The field is empty for a session with no color, and
    /// for an agent that assigns no color.
    ///
    /// The name is the fact. Which of the terminal's colors draws it is not
    /// settled here.
    pub color: String,
    /// The title that the session gave itself.
    pub title: String,
}

/// When a process started, as the system that runs it counts time.
///
/// The number means nothing anywhere else. It is ticks since boot on Linux,
/// microseconds since the epoch on macOS, and hundred-nanosecond intervals since
/// 1601 on Windows. Nothing reads the number. The code only compares it with
/// another number taken the same way on the same machine. That comparison is all
/// that it takes to tell one process from another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "native", derive(serde::Serialize, serde::Deserialize))]
pub struct Started(pub u64);

/// The process that an agent runs as: which process, and when it began.
///
/// A pid on its own does not name a process for long. The system hands the same
/// number out again after the process that held it ends. A record that remembers
/// only the number then points at whatever process inherited it. A question
/// about that stranger answers that the agent still runs. The start time is what
/// tells the two apart, because the system never hands out the pair twice.
///
/// `started` is `None` where the system did not say. The record then holds the
/// number alone, and it is exactly as credulous as before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "native", derive(serde::Serialize, serde::Deserialize))]
pub struct Process {
    pub pid: u32,
    pub started: Option<Started>,
}

/// What a line turned out to be.
///
/// A record written in another format is told apart from a line that is not a
/// record at all, because the two mean different things. The first says that the
/// two ends of the wire are out of step. The second says nothing.
///
/// The variants are of very different sizes, and the record is deliberately not
/// boxed. The code produces one record per line of every state message, and
/// matches it where it produces it. A pointer therefore costs an allocation per
/// record and buys nothing back.
#[allow(clippy::large_enum_variant)]
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
    /// The agent's own process, as its hook found it by a climb up its ancestry.
    /// `None` for a hook that did not say. Nothing can make sure that the
    /// process in such a record still runs.
    pub process: Option<Process>,
    pub turn: Turn,
    /// When the agent last called for the user, as the clock read at the time.
    /// It is taken once, where the call happens, so everything downstream orders
    /// the calls the same way, and compares no clock of its own.
    pub raised: u64,
}

/// Replace with a space every control character, and every character that can
/// split a record or a field.
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
            process: None,
            turn: Turn::default(),
            raised: 0,
        }
    }

    /// The record as one line: the format, then everything about the session.
    /// The title is last, because it is the one field that can hold anything at
    /// all.
    ///
    /// A process is written as its two halves rather than as one field. A record
    /// that names a process with no date therefore reads the same as a record
    /// from before the second half existed.
    pub fn encode(&self) -> String {
        let (pid, started) = match self.process {
            Some(process) => (
                process.pid.to_string(),
                process
                    .started
                    .map(|started| started.0.to_string())
                    .unwrap_or_default(),
            ),
            None => (String::new(), String::new()),
        };
        format!(
            "{FORMAT}{FIELD}{}{FIELD}{}{FIELD}{pid}{FIELD}{started}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}",
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
    /// The title is the whole remainder of the line, so a title with the field
    /// character in it still parses. No such title exists, because the
    /// constructor takes that character out.
    pub fn decode(line: &str) -> Record {
        let mut fields = line.splitn(12, FIELD);
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

    /// A record's fields, after the field that says which format they are in.
    fn read<'a>(mut fields: impl Iterator<Item = &'a str>) -> Option<Self> {
        let session = SessionId::new(fields.next()?)?;
        let agent = fields.next()?;
        let pid = fields.next()?;
        let started = fields.next()?;
        let turn = Turn::decode(fields.next()?)?;
        let raised = fields.next()?.parse().ok()?;
        let dir = fields.next()?.to_string();
        let name = fields.next()?.to_string();
        let color = fields.next()?.to_string();
        let origin = Origin::decode(fields.next()?);
        let title = fields.next()?.to_string();
        // A start time says nothing without the process that it belongs to. A
        // record that names no process therefore names none, whatever the start
        // time field holds.
        let process = match pid.is_empty() {
            true => None,
            false => Some(Process {
                pid: pid.parse().ok()?,
                started: match started.is_empty() {
                    true => None,
                    false => Some(Started(started.parse().ok()?)),
                },
            }),
        };
        Some(Agent {
            process,
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

    /// An origin that describes a zellij pane. A hook invoked in such a pane
    /// captures this origin.
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

    /// The record that an agent's hooks send when the turn changes: everything
    /// that the session is, plus whose turn it now is.
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
    fn a_process_survives_the_round_trip_and_so_does_having_none() {
        let with = Agent {
            process: Some(Process {
                pid: 4242,
                started: Some(Started(918_273)),
            }),
            ..agent("one", 3)
        };
        assert_eq!(Agent::decode(&with.encode()), Record::Known(with));
        let without = agent("two", 3);
        assert_eq!(without.process, None);
        assert_eq!(Agent::decode(&without.encode()), Record::Known(without));
    }

    #[test]
    fn a_process_nothing_could_date_keeps_its_number() {
        // The system does not always say when a process started. A record
        // without that answer still names the process that it found.
        let undated = Agent {
            process: Some(Process {
                pid: 4242,
                started: None,
            }),
            ..agent("one", 3)
        };
        assert_eq!(Agent::decode(&undated.encode()), Record::Known(undated));
    }

    #[test]
    fn a_start_time_without_a_process_is_no_process() {
        // Nothing writes this. A record that arrived with a start time and no
        // process names a moment with nothing to attach it to.
        let orphan = "4\tone\tclaude\t\t918273\tidle\t0\tdir\t\t\t\t";
        let Record::Known(read) = Agent::decode(orphan) else {
            panic!("not a record");
        };
        assert_eq!(read.process, None);
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
            "4\tone\tclaude",
            "4\tone\tclaude\t\t\tidle\t0\tdir",
            "4\tone\tclaude\tx\t\tidle\t0\tdir\t\t\t\ttitle",
            "4\tone\tclaude\t42\tnot-a-moment\tidle\t0\tdir\t\t\t\ttitle",
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
            Agent::decode("4\tone\tclaude\t\t\tdozing\t0\tdir\t\t\t\t"),
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
        // This is the difference that the header exists for. A state with
        // nothing in it is a message. Nothing at all is not a message.
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
