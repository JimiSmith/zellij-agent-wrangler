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
///
/// This number also covers the messages that a client sends back. A record
/// shape that did not change can still meet a daemon that expects something a
/// client of this age does not send. `ClientMessage::Beat` is that case: a
/// daemon keeps a client for as long as it speaks, so a client too old to beat
/// is dropped after a minute and a half, and the pane says nothing about why. A
/// reader that meets a number it does not know says so instead.
pub const FORMAT: u32 = 7;

/// The character that stands in for a record break on a transport that frames
/// its messages by the line.
///
/// A held `zellij pipe` and a named pipe both take one message per newline, and
/// every payload holds newlines, because [`RECORD`] is one. The ASCII record
/// separator carries that break instead. [`Origin`] uses the unit separator for
/// the fields inside one record, so the two agree rather than compete.
pub const ESCAPED_RECORD_BREAK: char = '\u{1e}';

/// One payload as one line, with every record break carried by
/// [`ESCAPED_RECORD_BREAK`].
pub fn escape_record_breaks(payload: &str) -> String {
    payload
        .chars()
        .map(|c| if c == RECORD { ESCAPED_RECORD_BREAK } else { c })
        .collect()
}

/// The payload that [`escape_record_breaks`] wrote, for a reader that took one
/// line off a line-framed transport.
///
/// This is not the exact reverse. The line arrives with the newline that framed
/// it still on the end. Zellij keeps that newline in the payload, so this drops
/// one of them before it restores the record breaks. Without that, every payload
/// ends in an empty record.
pub fn restore_record_breaks(line: &str) -> String {
    line.strip_suffix(RECORD)
        .unwrap_or(line)
        .chars()
        .map(|c| if c == ESCAPED_RECORD_BREAK { RECORD } else { c })
        .collect()
}

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
pub fn build_state_message(records: &str) -> String {
    format!("{HEADER} {FORMAT}{RECORD}{records}")
}

/// The format and the records of a state message, or `None` for anything that
/// is not one.
pub fn read_state_message(payload: &str) -> Option<(u32, &str)> {
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
pub struct LabelFacts {
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

/// What a session is working with: the branch it is on, the model it answers
/// with, and the context that its last answer spent. Every field is empty until
/// the agent's own files say otherwise.
///
/// These are facts, and not the line that a sidebar draws from them.
/// `StatusTemplate` spells that line, the way that [`label`] spells a label from
/// [`LabelFacts`].
///
/// [`label`]: crate::label::label
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusFacts {
    /// The branch that the working directory has checked out.
    pub branch: String,
    /// The model that the agent last answered with, under the name that the
    /// agent writes. How to spell that name for a reader is not settled here.
    pub model: String,
    /// The tokens that the last answer counted against the context window. Zero
    /// for a session that has answered nothing yet.
    pub context_tokens: u64,
}

/// The two transcript records that a client draws a preview from, as the daemon
/// read them. Each one is one line of JSON, or empty for a session that reports
/// none.
///
/// The daemon extracts nothing from either record. A client that later wants the
/// tool's input, the number of words or a second block reads it out of what it
/// already holds. The wire does not move again.
///
/// These records live here, and not beside the reader that finds them, because
/// [`session_facts`] is behind the feature that reads an agent's files. A
/// client is handed these records and builds without that feature.
///
/// [`session_facts`]: crate::session_facts
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranscriptRecords {
    /// The most recent `assistant` record whose content holds a text block with
    /// something in it.
    pub last_message: String,
    /// The `tool_use` record of the tool that runs now. Empty for an agent that
    /// runs none.
    pub running_tool: String,
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
pub struct ProcessStartStamp(pub u64);

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
    pub started: Option<ProcessStartStamp>,
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
    pub meta: LabelFacts,
    /// What the session works with. Empty until the agent's own files say
    /// otherwise. [`Agent::with_status`] attaches these facts.
    pub status: StatusFacts,
    /// What the session last said, and what it runs now. Empty until the agent's
    /// own files say otherwise. [`Agent::with_records`] attaches these records.
    pub records: TranscriptRecords,
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
    pub fn new(session: SessionId, agent: &str, meta: LabelFacts, origin: Origin) -> Self {
        Agent {
            session,
            agent: field(agent),
            meta: LabelFacts {
                dir: field(&meta.dir),
                name: field(&meta.name),
                color: field(&meta.color),
                title: field(&meta.title),
            },
            status: StatusFacts::default(),
            records: TranscriptRecords::default(),
            origin,
            process: None,
            turn: Turn::default(),
            raised: 0,
        }
    }

    /// This method attaches what the session is working with.
    ///
    /// Every text field loses the characters that can split a record, the same
    /// way that [`Agent::new`] treats a label fact. The token count is a number
    /// and can split nothing.
    pub fn with_status(mut self, status: StatusFacts) -> Self {
        self.status = StatusFacts {
            branch: field(&status.branch),
            model: field(&status.model),
            context_tokens: status.context_tokens,
        };
        self
    }

    /// This method attaches what the session last said, and what it runs now.
    ///
    /// Both records lose the characters that can split a record, the same way
    /// that [`Agent::new`] treats a label fact. A valid JSON line holds no
    /// control character, because JSON escapes every one of them, so this call
    /// changes nothing that the reader found. The call is what makes the
    /// guarantee hold for a line that turns out not to be JSON.
    pub fn with_records(mut self, records: TranscriptRecords) -> Self {
        self.records = TranscriptRecords {
            last_message: field(&records.last_message),
            running_tool: field(&records.running_tool),
        };
        self
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
            "{FORMAT}{FIELD}{}{FIELD}{}{FIELD}{pid}{FIELD}{started}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}",
            self.session.as_str(),
            self.agent,
            self.turn.encode(),
            self.raised,
            self.meta.dir,
            self.meta.name,
            self.meta.color,
            self.origin.encode(),
            self.status.branch,
            self.status.model,
            self.status.context_tokens,
            self.records.last_message,
            self.records.running_tool,
            self.meta.title,
        )
    }

    /// What a line turned out to be.
    ///
    /// The title is the whole remainder of the line, so a title with the field
    /// character in it still parses. No such title exists, because the
    /// constructor takes that character out.
    pub fn decode(line: &str) -> Record {
        let mut fields = line.splitn(17, FIELD);
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
        let branch = fields.next()?.to_string();
        let model = fields.next()?.to_string();
        let context_tokens = fields.next()?.parse().ok()?;
        let last_message = fields.next()?.to_string();
        let running_tool = fields.next()?.to_string();
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
                    false => Some(ProcessStartStamp(started.parse().ok()?)),
                },
            }),
        };
        Some(
            Agent {
                process,
                turn,
                raised,
                ..Agent::new(
                    session,
                    agent,
                    LabelFacts {
                        dir,
                        name,
                        color,
                        title,
                    },
                    origin,
                )
            }
            .with_status(StatusFacts {
                branch,
                model,
                context_tokens,
            })
            .with_records(TranscriptRecords {
                last_message,
                running_tool,
            }),
        )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::origin::Origin;

    /// A record line built from the fields that follow the format number. The
    /// helper counts the separators, so a test states what it means and a
    /// change to the number of fields lands in one place.
    pub(crate) fn record_line(fields: &[&str]) -> String {
        let mut line = FORMAT.to_string();
        for value in fields {
            line.push(FIELD);
            line.push_str(value);
        }
        line
    }

    /// The fields of a record that decodes, for a test that then spoils one of
    /// them. The record names a process, because a start time is read only for a
    /// record that names one.
    fn whole() -> Vec<&'static str> {
        vec![
            "one", "claude", "42", "918273", "idle", "0", "dir", "", "", "", "", "", "0", "", "",
            "title",
        ]
    }

    pub(crate) fn session(text: &str) -> SessionId {
        SessionId::new(text).unwrap()
    }

    pub(crate) fn meta(dir: &str, name: &str, title: &str) -> LabelFacts {
        LabelFacts {
            dir: dir.to_string(),
            name: name.to_string(),
            color: String::new(),
            title: title.to_string(),
        }
    }

    /// An origin that describes a zellij pane. A hook invoked in such a pane
    /// captures this origin.
    pub(crate) fn at_pane(pane: u32) -> Origin {
        Origin::from_lookup(|name| match name {
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
            LabelFacts {
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
                started: Some(ProcessStartStamp(918_273)),
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
        let orphan = record_line(&[
            "one", "claude", "", "918273", "idle", "0", "dir", "", "", "", "", "", "0", "", "", "",
        ]);
        let Record::Known(read) = Agent::decode(&orphan) else {
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
        let spoiled = |position: usize, value: &str| {
            let mut fields = whole();
            fields[position] = value;
            record_line(&fields)
        };
        for line in [
            String::new(),
            "one".to_string(),
            record_line(&["one", "claude"]),
            record_line(&["one", "claude", "", "", "idle", "0", "dir"]),
            spoiled(2, "x"),            // a pid that is not a number
            spoiled(3, "not-a-moment"), // a start time that is not a moment
            spoiled(12, ""),            // no count of the context spent
            spoiled(12, "many"),        // a count that is not a number
        ] {
            assert_eq!(Agent::decode(&line), Record::None, "{line}");
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
            Agent::decode(&record_line(&[
                "one", "claude", "", "", "dozing", "0", "dir", "", "", "", "", "", "0", "", "", "",
            ])),
            Record::None
        );
    }

    #[test]
    fn a_state_message_survives_the_round_trip() {
        let records = format!("{}\n{}", agent("one", 3).encode(), agent("two", 4).encode());
        let message = build_state_message(&records);
        assert_eq!(
            read_state_message(&message),
            Some((FORMAT, records.as_str()))
        );
    }

    #[test]
    fn having_no_agents_is_something_that_can_be_said() {
        // This is the difference that the header exists for. A state with
        // nothing in it is a message. Nothing at all is not a message.
        let empty = build_state_message("");
        assert_eq!(read_state_message(&empty), Some((FORMAT, "")));
        assert_eq!(read_state_message(""), None);
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
            assert_eq!(read_state_message(text), None, "{text}");
        }
    }

    #[test]
    fn a_state_message_says_which_format_it_is_in() {
        let older = "wrangler 1\n3\tone\tclaude";
        assert_eq!(read_state_message(older), Some((1, "3\tone\tclaude")));
    }

    #[test]
    fn a_color_survives_the_round_trip() {
        let record = colored("one", "purple");
        assert_eq!(Agent::decode(&record.encode()), Record::Known(record));
    }

    #[test]
    fn a_status_survives_the_round_trip_and_so_does_having_none() {
        let working = agent("one", 3).with_status(StatusFacts {
            branch: "main".to_string(),
            model: "claude-opus-5".to_string(),
            context_tokens: 195_547,
        });
        assert_eq!(Agent::decode(&working.encode()), Record::Known(working));
        let quiet = agent("two", 3);
        assert_eq!(quiet.status, StatusFacts::default());
        assert_eq!(Agent::decode(&quiet.encode()), Record::Known(quiet));
    }

    /// One `assistant` record with one text block, as Claude writes it.
    fn text_record(said: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"model":"claude-opus-5","content":[{{"type":"text","text":"{said}"}}]}}}}"#
        )
    }

    #[test]
    fn both_transcript_records_survive_the_round_trip_and_so_does_having_neither() {
        let talking = agent("one", 3).with_records(TranscriptRecords {
            last_message: text_record("the port is done"),
            running_tool: r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Bash"}]}}"#.to_string(),
        });
        assert_eq!(Agent::decode(&talking.encode()), Record::Known(talking));
        let quiet = agent("two", 3);
        assert_eq!(quiet.records, TranscriptRecords::default());
        assert_eq!(Agent::decode(&quiet.encode()), Record::Known(quiet));
    }

    #[test]
    fn a_transcript_record_cannot_split_the_record_it_sits_in() {
        // JSON escapes every character that can split a line, so a record
        // that the reader found is already safe. A line that turns out not to
        // be JSON is made safe here.
        let record = agent("one", 3).with_records(TranscriptRecords {
            last_message: "a\tb\nc".to_string(),
            running_tool: "d\te".to_string(),
        });
        assert_eq!(record.records.last_message, "a b c");
        assert_eq!(record.records.running_tool, "d e");
        assert_eq!(Agent::decode(&record.encode()), Record::Known(record));
    }

    #[test]
    fn a_title_after_two_transcript_records_is_still_the_rest_of_the_line() {
        // The title is the remainder of the line, and the two records sit in
        // front of it. A record that ended one field early would take the title
        // with it.
        let record = Agent::new(
            session("one"),
            "claude",
            meta("d", "", "the port"),
            at_pane(1),
        )
        .with_records(TranscriptRecords {
            last_message: text_record("said something"),
            running_tool: String::new(),
        });
        let Record::Known(read) = Agent::decode(&record.encode()) else {
            panic!("not a record");
        };
        assert_eq!(read.meta.title, "the port");
        assert_eq!(read.records.last_message, text_record("said something"));
        assert_eq!(read.records.running_tool, "");
    }

    #[test]
    fn a_record_of_the_format_before_this_one_says_which_format_it_is() {
        // The two ends of the wire are installed separately. A reader that made
        // nothing of the line cannot tell the user why the pane is empty.
        let older =
            agent("one", 3)
                .encode()
                .replacen(&FORMAT.to_string(), &(FORMAT - 1).to_string(), 1);
        assert_eq!(Agent::decode(&older), Record::Foreign(FORMAT - 1));
    }

    #[test]
    fn a_status_fact_cannot_split_the_record_it_sits_in() {
        let record = agent("one", 3).with_status(StatusFacts {
            branch: "a\tb\nc".to_string(),
            model: "d\te".to_string(),
            context_tokens: 7,
        });
        assert_eq!(record.status.branch, "a b c");
        assert_eq!(record.status.model, "d e");
        assert_eq!(Agent::decode(&record.encode()), Record::Known(record));
    }

    #[test]
    fn a_run_of_records_makes_one_line_and_comes_back_whole() {
        // Every payload holds record breaks, so this is the ordinary case and
        // not the awkward one.
        let payload = build_state_message("3\tone\tclaude\n3\ttwo\tcopilot");
        let line = escape_record_breaks(&payload);
        assert!(!line.contains(RECORD), "one line, whatever it carries");
        assert_eq!(restore_record_breaks(&line), payload);
    }

    #[test]
    fn the_newline_that_framed_a_line_is_not_part_of_the_payload() {
        // A line-framed transport keeps the newline that ended the message. A
        // reader that leaves it there reads one empty record at the end.
        let payload = build_state_message("3\tone\tclaude");
        let framed = format!("{}\n", escape_record_breaks(&payload));
        assert_eq!(restore_record_breaks(&framed), payload);
    }

    #[test]
    fn a_payload_with_no_record_break_is_carried_unchanged() {
        assert_eq!(escape_record_breaks("wrangler 4"), "wrangler 4");
        assert_eq!(restore_record_breaks("wrangler 4"), "wrangler 4");
    }
}
