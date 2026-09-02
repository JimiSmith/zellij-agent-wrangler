//! What a client draws under a dashboard row, read out of the two raw records
//! that the daemon sent.
//!
//! The daemon extracts nothing from either record. It sends both as it read
//! them, so a client that wants another field reads it here rather than on the
//! wire. This module is the client's half of that bargain.
//!
//! Nothing here fails. A line that is not JSON, and a record of a shape this
//! code does not know, give an empty preview. That is the same answer as a
//! session that reports no records at all.

use serde_json::Value;

use crate::agent::TranscriptRecords;

/// The input key that says what a tool works on, for every tool that names one.
///
/// A tool call carries an `input` object whose keys the tool itself decides,
/// and no key is common to every tool. A tool that this table does not name
/// therefore draws its name alone. Reading whatever key a strange tool happens
/// to carry would be a guess, and an MCP tool promises nothing.
const ARGUMENT_KEYS: &[(&str, &str)] = &[
    ("Agent", "description"),
    ("Bash", "command"),
    ("Edit", "file_path"),
    ("Glob", "pattern"),
    ("Grep", "pattern"),
    ("NotebookEdit", "notebook_path"),
    ("Read", "file_path"),
    ("Skill", "skill"),
    ("Task", "description"),
    ("WebFetch", "url"),
    ("WebSearch", "query"),
    ("Write", "file_path"),
];

/// The columns of a timestamp that name the date: `2026-09-01`.
const DATE_COLUMNS: usize = 10;

/// The column that a timestamp writes `T` in, between the date and the time.
const DATE_TIME_SEPARATOR: usize = 10;

/// The column that a timestamp writes the last of the minutes in.
const MINUTE_END: usize = 16;

/// One tool call, as much of it as a preview says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolCall {
    /// What the tool is called, as the call spells it.
    pub name: String,
    /// The one input that says what the tool works on. Empty for a tool that
    /// [`ARGUMENT_KEYS`] does not name, and for a call whose input lacks the
    /// key that the table names.
    pub argument: String,
}

/// What a client draws under a dashboard row.
///
/// No field holds a control character other than the line breaks of the
/// message. See [`without_control_characters`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preview {
    /// What the agent last told the user, as the agent wrote it. Empty for a
    /// session that reports no record, and for a record that this code cannot
    /// read.
    ///
    /// The line breaks are kept, and every other control character is a space.
    /// An agent writes markdown, and a heading, a list item and a code fence
    /// each need the line that they start.
    pub message: String,
    /// When the agent wrote that message, spelled `2026-09-01 05:11Z`. Empty
    /// for a record that carries no timestamp of that shape.
    pub timestamp: String,
    /// The tool that runs now. `None` for an agent that runs none.
    pub running_tool: Option<ToolCall>,
}

/// A record's string field, or `None` when it is absent or empty.
pub(crate) fn string_field<'a>(record: &'a Value, key: &str) -> Option<&'a str> {
    record
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

/// The content blocks of one record, or nothing for a record that carries none.
///
/// Claude writes one block per record today. The reader still walks every
/// block, because a record that carried two would otherwise report only the
/// first one.
pub(crate) fn content_blocks(record: &Value) -> &[Value] {
    record
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// Whether a block is text with something in it.
///
/// A thinking block is not text. Claude writes the signature of its reasoning
/// and not the reasoning, so such a block carries nothing that a reader can
/// draw.
pub(crate) fn holds_text(block: &Value) -> bool {
    string_field(block, "type") == Some("text") && string_field(block, "text").is_some()
}

/// `text` with every control character replaced by a space.
///
/// A record travels as one line, so the daemon already replaced the control
/// characters that the line itself carried. JSON writes a line break inside a
/// string as two characters, and reading the string turns those two back into
/// one control character. So a message and a tool argument both reach a client
/// with real line breaks in them.
///
/// A tool name and a tool argument are drawn on one terminal line, so they take
/// this. A message keeps its line breaks and takes
/// [`without_control_characters_except_line_breaks`].
fn without_control_characters(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// `text` with every control character replaced by a space, except the line
/// breaks, which are kept.
///
/// A client reads the message as markdown, and the markdown of a heading, a
/// list item and a code fence starts a line. A carriage return before a line
/// break becomes a space, and markdown ignores one space at the end of a
/// line.
fn without_control_characters_except_line_breaks(text: &str) -> String {
    text.chars()
        .map(|c| match c.is_control() && c != '\n' {
            true => ' ',
            false => c,
        })
        .collect()
}

/// One line of JSON, or `None` for a line that is empty or does not parse.
fn parse_record(line: &str) -> Option<Value> {
    match line.is_empty() {
        true => None,
        false => serde_json::from_str(line).ok(),
    }
}

/// The date and the minute of a record's timestamp, spelled `2026-09-01 05:11Z`.
///
/// A record spells its timestamp `2026-09-01T05:11:01.469Z`, which is UTC. The
/// `Z` is kept, because a client knows no offset from UTC and must not let a
/// reader take the time for a local one.
fn timestamp(record: &Value) -> String {
    let Some(stamp) = string_field(record, "timestamp") else {
        return String::new();
    };
    let columns: Vec<char> = stamp.chars().collect();
    if columns.len() < MINUTE_END || columns[DATE_TIME_SEPARATOR] != 'T' {
        return String::new();
    }
    let date: String = columns[..DATE_COLUMNS].iter().collect();
    let minute: String = columns[DATE_TIME_SEPARATOR + 1..MINUTE_END]
        .iter()
        .collect();
    format!("{date} {minute}Z")
}

/// Every text block of a record, joined by a space.
///
/// Claude writes one block per record. A record that carried a second text
/// block would otherwise draw only the first one.
fn message(record: &Value) -> String {
    let blocks: Vec<&str> = content_blocks(record)
        .iter()
        .filter(|block| holds_text(block))
        .filter_map(|block| string_field(block, "text"))
        .collect();
    without_control_characters_except_line_breaks(&blocks.join(" "))
}

/// The tool that a record calls, for a record that calls one.
fn tool_call(record: &Value) -> Option<ToolCall> {
    let block = content_blocks(record)
        .iter()
        .find(|block| string_field(block, "type") == Some("tool_use"))?;
    let name = string_field(block, "name")?;
    let key = ARGUMENT_KEYS
        .iter()
        .find(|(tool, _)| *tool == name)
        .map(|(_, key)| *key);
    let argument = key
        .and_then(|key| {
            block
                .get("input")
                .and_then(|input| string_field(input, key))
        })
        .unwrap_or_default();
    Some(ToolCall {
        name: without_control_characters(name),
        argument: without_control_characters(argument),
    })
}

impl Preview {
    /// What the two records hold, as much of it as a client draws.
    pub fn from_records(records: &TranscriptRecords) -> Preview {
        let written = parse_record(&records.last_message);
        Preview {
            message: written.as_ref().map(message).unwrap_or_default(),
            timestamp: written.as_ref().map(timestamp).unwrap_or_default(),
            running_tool: parse_record(&records.running_tool)
                .as_ref()
                .and_then(tool_call),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The records that a session reports, from the two lines it reports them
    /// as.
    fn records(last_message: &str, running_tool: &str) -> TranscriptRecords {
        TranscriptRecords {
            last_message: last_message.to_string(),
            running_tool: running_tool.to_string(),
        }
    }

    /// An `assistant` record carrying one text block, and the timestamp that
    /// Claude writes on every record.
    fn text_record(text: &str) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"2026-09-01T05:11:01.469Z","message":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    }

    /// An `assistant` record calling `name` with one input key.
    fn tool_record(name: &str, key: &str, value: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"toolu_one","name":"{name}","input":{{"{key}":"{value}"}}}}]}}}}"#
        )
    }

    #[test]
    fn a_text_record_gives_what_the_agent_said_and_when_it_said_it() {
        let preview = Preview::from_records(&records(&text_record("the port is done"), ""));
        assert_eq!(preview.message, "the port is done");
        assert_eq!(preview.timestamp, "2026-09-01 05:11Z");
        assert_eq!(preview.running_tool, None);
    }

    #[test]
    fn a_record_carrying_two_text_blocks_joins_them() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"one"},{"type":"thinking","thinking":""},{"type":"text","text":"two"}]}}"#;
        assert_eq!(Preview::from_records(&records(line, "")).message, "one two");
    }

    #[test]
    fn a_line_break_inside_a_message_reaches_the_client() {
        // A record travels as one line, so the daemon replaced the control
        // characters that the line held. JSON writes a line break as two
        // characters, and reading the string turns those two into one. The
        // client reads the message as markdown, so the break is kept.
        let line =
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"one\ntwo"}]}}"#;
        assert_eq!(
            Preview::from_records(&records(line, "")).message,
            "one\ntwo"
        );
    }

    #[test]
    fn a_tab_inside_a_message_is_drawn_as_a_space() {
        let line =
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"one\ttwo"}]}}"#;
        assert_eq!(Preview::from_records(&records(line, "")).message, "one two");
    }

    #[test]
    fn a_record_with_no_timestamp_says_nothing_about_when() {
        let line =
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"a message"}]}}"#;
        let preview = Preview::from_records(&records(line, ""));
        assert_eq!(preview.message, "a message");
        assert_eq!(preview.timestamp, "");
    }

    #[test]
    fn a_timestamp_of_another_shape_says_nothing_about_when() {
        for stamp in ["", "2026-09-01", "Tuesday", "2026-09-01 05:11:01Z"] {
            let line = format!(
                r#"{{"type":"assistant","timestamp":"{stamp}","message":{{"content":[]}}}}"#
            );
            assert_eq!(
                Preview::from_records(&records(&line, "")).timestamp,
                "",
                "{stamp}"
            );
        }
    }

    #[test]
    fn a_tool_draws_the_one_input_key_that_its_own_tool_names() {
        for (name, key, value) in [
            ("Bash", "command", "cargo test --workspace"),
            (
                "Read",
                "file_path",
                "crates/agent-wrangler-ui/src/render.rs",
            ),
            ("Write", "file_path", "PROGRESS.md"),
            ("Edit", "file_path", "CLAUDE.md"),
            ("WebFetch", "url", "https://zellij.dev"),
            ("Skill", "skill", "diff-viewer:review"),
        ] {
            let preview = Preview::from_records(&records("", &tool_record(name, key, value)));
            assert_eq!(
                preview.running_tool,
                Some(ToolCall {
                    name: name.to_string(),
                    argument: value.to_string(),
                }),
                "{name}"
            );
        }
    }

    #[test]
    fn a_tool_that_the_table_does_not_name_gives_its_name_alone() {
        let line = tool_record("mcp__playwright__browser_click", "element", "the button");
        let preview = Preview::from_records(&records("", &line));
        assert_eq!(
            preview.running_tool,
            Some(ToolCall {
                name: "mcp__playwright__browser_click".to_string(),
                argument: String::new(),
            })
        );
    }

    #[test]
    fn a_call_whose_input_lacks_the_key_gives_no_argument() {
        let line = tool_record("Bash", "description", "list the repository");
        assert_eq!(
            Preview::from_records(&records("", &line)).running_tool,
            Some(ToolCall {
                name: "Bash".to_string(),
                argument: String::new(),
            })
        );
    }

    #[test]
    fn a_line_break_inside_a_tool_argument_is_drawn_as_a_space() {
        let line = tool_record("Bash", "command", "cargo fmt\\ncargo test");
        assert_eq!(
            Preview::from_records(&records("", &line)).running_tool,
            Some(ToolCall {
                name: "Bash".to_string(),
                argument: "cargo fmt cargo test".to_string(),
            })
        );
    }

    #[test]
    fn a_record_that_calls_no_tool_reports_none() {
        let line = text_record("nothing runs");
        assert_eq!(
            Preview::from_records(&records("", &line)).running_tool,
            None
        );
    }

    #[test]
    fn a_line_that_is_not_json_gives_an_empty_preview() {
        assert_eq!(
            Preview::from_records(&records("not a record", "not a record")),
            Preview::default()
        );
    }

    #[test]
    fn a_session_that_reports_no_records_gives_an_empty_preview() {
        assert_eq!(
            Preview::from_records(&TranscriptRecords::default()),
            Preview::default()
        );
    }
}
