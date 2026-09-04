//! What an agent's own files say about one session.
//!
//! A reader here returns four groups of facts.
//!
//! - The label: the name, the title and the color.
//! - The status: the branch, the model and the context.
//! - The records: the last message, and the tool that still runs.
//! - The parent, for a child that another child started.
//!
//! No agent puts any of that in a hook body. A reader therefore takes it off
//! disk at the moment when a hook fires. That is what keeps a row current. An
//! agent fires hooks throughout its turn, and each hook is another look.
//!
//! Nothing here fails. A file that is missing, unreadable or unexpected gives
//! empty facts. That is the same answer as a session that has said nothing yet.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::agent::{AgentId, LabelFacts, StatusFacts, TranscriptRecords};
// The reader that finds a record and the reader that draws it ask the same
// three questions of it. Those three live beside the drawing's reader, which is
// behind `json`, and `native` takes `json` with it.
use crate::preview::{content_blocks, holds_text, string_field};

/// Everything that an agent's own files say about one session: what it is
/// called by, and what it works with.
///
/// The two travel together because one read finds both. A caller that wants
/// only a label still pays for one read and not two.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionFacts {
    pub label: LabelFacts,
    pub status: StatusFacts,
    pub records: TranscriptRecords,
    /// The agent that started this child, for a child that another child
    /// started. Claude writes `parentAgentId` exactly when the parent is not
    /// the session, so `None` says that the session started this agent.
    ///
    /// A session and a Copilot agent report `None` here, because neither reads
    /// a meta file.
    pub parent: Option<AgentId>,
}

/// The model that Claude writes for a message it composed itself rather than
/// asked a model for. Such a message reports no model that the user ran.
const SYNTHETIC: &str = "<synthetic>";

/// How much of a transcript's end is read. A fixed amount keeps the cost of a
/// hook the same, however long the session runs.
///
/// Only the end is read. What a session records once, as it begins, falls out of
/// this window as the session grows, and nothing looks for it again. A record
/// carries what the client found this time. A record that finds nothing says
/// nothing, and the sidebar keeps what it already knew. A color must be found
/// once, early, and never again.
const TAIL: u64 = 64 * 1024;

/// The last `TAIL` bytes of a file, and whether anything was cut off the front
/// of them. Nothing at all when the file cannot be read.
fn read_tail(path: &Path) -> Option<(Vec<u8>, bool)> {
    let mut file = std::fs::File::open(path).ok()?;
    let size = file.seek(SeekFrom::End(0)).ok()?;
    file.seek(SeekFrom::Start(size.saturating_sub(TAIL))).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some((bytes, size > TAIL))
}

/// The id of a tool call, for a block that is one.
///
/// A call with no id cannot be paired with its result. Nothing tracks such a
/// call, which would otherwise be reported as running for the rest of the
/// session.
fn tool_call_id(block: &Value) -> Option<&str> {
    match string_field(block, "type") {
        Some("tool_use") => string_field(block, "id"),
        _ => None,
    }
}

/// The id of the tool call that a block answers, for a block that answers one.
fn answered_call_id(block: &Value) -> Option<&str> {
    match string_field(block, "type") {
        Some("tool_result") => string_field(block, "tool_use_id"),
        _ => None,
    }
}

/// What a session works with, read out of one record of type `assistant`.
///
/// Claude writes one such record for every reply it gives. The record names the
/// branch that the working directory had checked out, the model that composed
/// the reply, and what the reply counted against the context window. The count
/// adds every kind of token that the window holds, cached ones included.
///
/// The answer is `None` for a record that no model composed. A record of
/// another type names no model, and Claude writes the synthetic model on a
/// message that it composed for itself after a request failed. Neither one says
/// what the session works with.
fn status_from_assistant_record(record: &Value) -> Option<StatusFacts> {
    let message = record.get("message")?;
    let model = string_field(message, "model")?;
    if model == SYNTHETIC {
        return None;
    }
    let counted = |key: &str| {
        message
            .get("usage")
            .and_then(|usage| usage.get(key))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    Some(StatusFacts {
        branch: string_field(record, "gitBranch")
            .unwrap_or_default()
            .to_string(),
        model: model.to_string(),
        context_tokens: counted("input_tokens")
            + counted("cache_creation_input_tokens")
            + counted("cache_read_input_tokens")
            + counted("output_tokens"),
    })
}

/// What a Claude session is called, and what it works with, read from its
/// transcript.
///
/// The transcript records two kinds of title: the title that the user gave, and
/// the title that Claude wrote for itself. A given title wins wherever both
/// appear, because the user asked for it. Between two titles of a kind, the
/// later one wins, because it is the later name.
///
/// The color is read the same way as a title that Claude wrote for itself. The
/// last color in the window is the one in force.
///
/// A teammate's own name rides on every conversation record that it writes, so
/// the first name found in the window answers. The record that renames a
/// teammate carries the same field, and the reader passes over it. That record
/// says what the name became, and the conversation records after it already say
/// so.
///
/// What the session works with comes from the last `assistant` record in the
/// window. All three of those values ride on that one record, so the three
/// always describe the same moment. A window holding no such record reports none
/// of them, which is what a session that has replied nothing reports.
///
/// Two records travel as they were read, for a client to draw a preview from.
/// The first is the most recent `assistant` record that says something. About
/// half of the assistant records in a transcript are a tool call and nothing
/// else. A preview off the last record alone would therefore draw blank for an
/// agent that is mid-turn. That is exactly the agent a user asks about.
///
/// A record that Claude composed for itself counts as one that says something,
/// although [`status_from_assistant_record`] passes over the same record. Such a
/// record names no model that the user ran, so it says nothing about what the
/// session works with. It does say why the session stopped, and a preview
/// exists to show exactly that.
///
/// The second record is the tool call that no result answers yet. A call and its
/// result name each other by id, so two tools that an agent started together
/// stay apart, whichever one comes back first. The reader reports the oldest
/// call still unanswered, because that tool started first and still runs.
pub fn read_claude_session(transcript: &str) -> SessionFacts {
    let Some((bytes, cut)) = read_tail(Path::new(transcript)) else {
        return SessionFacts::default();
    };
    let mut lines = bytes.split(|byte| *byte == b'\n');
    // The first line of a window that starts mid-file is half a record.
    if cut {
        lines.next();
    }

    let (mut given, mut written) = (String::new(), String::new());
    let (mut name, mut color) = (String::new(), String::new());
    let mut status = StatusFacts::default();
    let mut last_message = String::new();
    // Every tool call in this window that no result answers yet, oldest first.
    // Each entry holds the id that the call carries, and the record as read.
    let mut unpaired: Vec<(String, String)> = Vec::new();
    for line in lines {
        // A JSON parse of every record parses the whole conversation. The
        // records that matter name themselves in bytes, and a search for those
        // bytes comes first.
        let wanted = [
            b"\"custom-title\"".as_slice(),
            b"\"ai-title\"",
            b"\"agent-color\"",
            b"\"agentName\"",
            b"\"assistant\"",
            b"\"tool_result\"",
        ]
        .iter()
        .any(|marker| holds_bytes(line, marker));
        if !wanted {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        match string_field(&record, "type") {
            Some("custom-title") => {
                given = string_field(&record, "customTitle")
                    .unwrap_or("")
                    .to_string()
            }
            Some("ai-title") => {
                written = string_field(&record, "aiTitle").unwrap_or("").to_string()
            }
            Some("agent-color") => {
                color = string_field(&record, "agentColor")
                    .unwrap_or("")
                    .to_string()
            }
            Some("agent-name") => {}
            Some("assistant") => {
                if name.is_empty() {
                    name = string_field(&record, "agentName").unwrap_or("").to_string();
                }
                // The later record wins, the way the later title does.
                if let Some(found) = status_from_assistant_record(&record) {
                    status = found;
                }
                for block in content_blocks(&record) {
                    if holds_text(block) {
                        last_message = String::from_utf8_lossy(line).into_owned();
                    }
                    if let Some(id) = tool_call_id(block) {
                        unpaired.push((id.to_string(), String::from_utf8_lossy(line).into_owned()));
                    }
                }
            }
            _ if name.is_empty() => {
                name = string_field(&record, "agentName").unwrap_or("").to_string()
            }
            _ => {}
        }
        // A result answers one call by id. A result for a call that was issued
        // before this window opens names nothing here, and is dropped.
        for block in content_blocks(&record) {
            if let Some(answered) = answered_call_id(block) {
                unpaired.retain(|(id, _)| id != answered);
            }
        }
    }

    SessionFacts {
        label: LabelFacts {
            dir: String::new(),
            name,
            color,
            title: if given.is_empty() { written } else { given },
        },
        status,
        records: TranscriptRecords {
            last_message,
            running_tool: unpaired
                .into_iter()
                .next()
                .map(|(_, record)| record)
                .unwrap_or_default(),
        },
        // A transcript never names a parent. Only a meta file does, and only a
        // child has one. [`read_claude_child`] fills this in afterwards.
        parent: None,
    }
}

/// Whether `haystack` holds `needle`.
fn holds_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Where Copilot keeps what it knows about one session.
fn workspace_path(home: &Path, session: &str) -> PathBuf {
    home.join(".copilot")
        .join("session-state")
        .join(session)
        .join("workspace.yaml")
}

/// What a Copilot session is called: the name that it was given, or the summary
/// that it wrote for itself.
///
/// Copilot has no teammates of its own, so a session read here is always a
/// session of its own.
///
/// Copilot records none of the values that a status line draws, so the status
/// of a Copilot session is empty. The daemon never opens a Copilot transcript,
/// so such a session reports no transcript records either, and nothing here
/// guesses at them.
/// Where Claude keeps the two files of one child of a session.
///
/// The two are built together, because a caller that reads one always reads the
/// other. [`ChildPaths`] therefore holds a pair and never one half.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildPaths {
    /// What the child writes its own conversation to.
    pub transcript: String,
    /// The file that names the child. Claude spells it `.meta.json`.
    pub meta_file: String,
}

/// Where Claude keeps the files of one child of the session that
/// `lead_transcript` belongs to.
///
/// Claude reports the transcript of the lead on every hook, and never the file
/// of a child. So the daemon builds the pair itself. The files sit under a
/// directory named for the lead session, beside the lead's own file:
///
/// ```text
/// <project directory>/<lead session id>.jsonl
/// <project directory>/<lead session id>/subagents/agent-<agent id>.jsonl
/// <project directory>/<lead session id>/subagents/agent-<agent id>.meta.json
/// ```
///
/// Measurement on Claude Code 2.1.258 confirmed the rule. A `SubagentStop` body
/// carries `agent_transcript_path`, and its value is the path that this function
/// builds.
///
/// The result is nothing for a lead transcript with no file name, and for an
/// agent id with no characters at all.
pub fn claude_child_paths(lead_transcript: &str, agent_id: &str) -> Option<ChildPaths> {
    if agent_id.is_empty() {
        return None;
    }
    let lead = Path::new(lead_transcript);
    let directory = lead.parent()?.join(lead.file_stem()?).join("subagents");
    Some(ChildPaths {
        transcript: directory
            .join(format!("agent-{agent_id}.jsonl"))
            .to_str()?
            .to_string(),
        meta_file: directory
            .join(format!("agent-{agent_id}.meta.json"))
            .to_str()?
            .to_string(),
    })
}

/// What a child's own two files say about it.
///
/// The transcript of a child holds the branch, the model, the context, the last
/// message and the running tool, in the records that
/// [`read_claude_session`] already reads. It holds no name, no title, no color
/// and no parent, so the meta file supplies those four.
///
/// A file that is not there says nothing, and this function guesses nothing.
pub fn read_claude_child(paths: &ChildPaths) -> SessionFacts {
    let mut facts = read_claude_session(&paths.transcript);
    let Ok(text) = std::fs::read_to_string(Path::new(&paths.meta_file)) else {
        return facts;
    };
    let Ok(meta) = serde_json::from_str::<Value>(&text) else {
        return facts;
    };
    // `agentType` is the built in type of a subagent, and the name that the lead
    // gave a teammate. Either one is what a user calls the child.
    facts.label.name = string_field(&meta, "agentType").unwrap_or("").to_string();
    facts.label.title = string_field(&meta, "description").unwrap_or("").to_string();
    // A teammate is a session of its own and names its own color. A subagent
    // names none, and the daemon fills it from the agent that started it.
    facts.label.color = string_field(&meta, "color").unwrap_or("").to_string();
    // Claude writes `parentAgentId` only when the parent is not the session.
    // Measurement on Claude Code 2.1.258 found it on a subagent of a teammate,
    // and on nothing else.
    facts.parent = string_field(&meta, "parentAgentId").and_then(AgentId::new);
    facts
}

pub fn read_copilot_session(home: &Path, session: &str) -> SessionFacts {
    let Ok(text) = std::fs::read_to_string(workspace_path(home, session)) else {
        return SessionFacts::default();
    };
    let title = match yaml_field(&text, "name") {
        title if title.is_empty() => yaml_field(&text, "summary"),
        title => title,
    };
    SessionFacts {
        label: LabelFacts {
            title,
            ..LabelFacts::default()
        },
        status: StatusFacts::default(),
        records: TranscriptRecords::default(),
        // Copilot has subagent events of its own, and refinement measured none
        // of them. A Copilot agent therefore starts nothing that the daemon
        // draws under it.
        parent: None,
    }
}

/// One top-level field of a workspace file, reduced to a single line.
///
/// The reader undoes only the quotes that a written-out YAML scalar uses. A
/// value written as a block is taken as its first non-empty line. A title is one
/// line of text, and the rest of a block is not part of it.
fn yaml_field(text: &str, key: &str) -> String {
    let prefix = format!("{key}:");
    let mut lines = text.lines();
    let Some(first) = lines.find_map(|line| line.strip_prefix(&prefix)) else {
        return String::new();
    };
    let first = first.trim();
    if !matches!(first, "|" | "|-" | "|+" | ">" | ">-" | ">+") {
        return yaml_scalar(first);
    }
    lines
        // A line that starts in the first column is outside the block.
        .take_while(|line| line.starts_with(char::is_whitespace) || line.trim().is_empty())
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string()
}

/// A YAML scalar as its text: quotes taken off, and the words for nothing read
/// as nothing.
fn yaml_scalar(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() || value == "null" || value == "~" {
        return String::new();
    }
    if let Some(inner) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        return match serde_json::from_str::<Value>(value) {
            Ok(Value::String(text)) => text,
            _ => inner.to_string(),
        };
    }
    match value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        Some(inner) => inner.replace("''", "'"),
        None => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes one transcript of the given lines, and gives back its path.
    fn write_transcript(dir: &Path, lines: &[&str]) -> String {
        let path = dir.join("transcript.jsonl");
        std::fs::write(&path, lines.join("\n")).unwrap();
        path.to_string_lossy().to_string()
    }

    /// A directory of this test's own, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("agent-wrangler-{name}"));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Scratch(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A lead transcript, plus the two files of one child under it.
    fn write_child(dir: &Path, agent_id: &str, meta: &str, lines: &[&str]) -> ChildPaths {
        let lead = dir.join("4630d1cb.jsonl");
        std::fs::write(&lead, "").unwrap();
        let paths = claude_child_paths(&lead.to_string_lossy(), agent_id).unwrap();
        std::fs::create_dir_all(Path::new(&paths.transcript).parent().unwrap()).unwrap();
        std::fs::write(&paths.transcript, lines.join("\n")).unwrap();
        if !meta.is_empty() {
            std::fs::write(&paths.meta_file, meta).unwrap();
        }
        paths
    }

    #[test]
    fn the_files_of_a_child_sit_under_a_directory_named_for_the_lead() {
        let paths = claude_child_paths("/p/4630d1cb.jsonl", "a9a352ae014362aad").unwrap();
        assert!(paths.transcript.ends_with("agent-a9a352ae014362aad.jsonl"));
        assert!(paths
            .meta_file
            .ends_with("agent-a9a352ae014362aad.meta.json"));
        // The directory of the lead, then the lead session, then subagents.
        for path in [&paths.transcript, &paths.meta_file] {
            let parent = Path::new(path).parent().unwrap();
            assert_eq!(parent.file_name().unwrap(), "subagents");
            assert_eq!(parent.parent().unwrap().file_name().unwrap(), "4630d1cb");
        }
    }

    #[test]
    fn an_agent_id_with_no_characters_names_no_files() {
        assert_eq!(claude_child_paths("/p/4630d1cb.jsonl", ""), None);
    }

    #[test]
    fn a_child_takes_its_name_and_its_title_from_the_meta_file() {
        let scratch = Scratch::new("child-meta");
        let paths = write_child(
            scratch.path(),
            "a9a352",
            r#"{"agentType":"Explore","description":"Explore the tmux client crate"}"#,
            &[r#"{"type":"user","message":"hello"}"#],
        );
        let facts = read_claude_child(&paths);
        assert_eq!(facts.label.name, "Explore");
        assert_eq!(facts.label.title, "Explore the tmux client crate");
    }

    #[test]
    fn a_teammate_takes_its_color_and_a_subagent_takes_none() {
        // Measured on Claude Code 2.1.258. A teammate meta file carries color,
        // name and taskKind. A subagent meta file carries none of the three.
        let scratch = Scratch::new("child-color");
        let teammate = write_child(
            scratch.path(),
            "adepth-probe-7b57",
            concat!(
                r#"{"agentType":"depth-probe","description":"Measure depth two","#,
                r#""name":"depth-probe","taskKind":"in_process_teammate","color":"purple"}"#
            ),
            &[r#"{"type":"user","message":"hello"}"#],
        );
        assert_eq!(read_claude_child(&teammate).label.color, "purple");
        let subagent = write_child(
            scratch.path(),
            "ae4c3164",
            r#"{"agentType":"Explore","description":"a look","toolUseId":"t1"}"#,
            &[r#"{"type":"user","message":"hello"}"#],
        );
        assert_eq!(read_claude_child(&subagent).label.color, "");
    }

    #[test]
    fn a_child_of_a_child_names_its_parent() {
        // Claude writes parentAgentId exactly when the parent is not the
        // session. A subagent that the lead itself started carries none.
        let scratch = Scratch::new("child-parent");
        let nested = write_child(
            scratch.path(),
            "ae4c3164",
            concat!(
                r#"{"agentType":"Explore","description":"a look","#,
                r#""parentAgentId":"adepth-probe-7b57","spawnDepth":1}"#
            ),
            &[r#"{"type":"user","message":"hello"}"#],
        );
        assert_eq!(
            read_claude_child(&nested).parent,
            AgentId::new("adepth-probe-7b57")
        );
        let plain = write_child(
            scratch.path(),
            "ab1234",
            r#"{"agentType":"Explore","description":"a look","toolUseId":"t1"}"#,
            &[r#"{"type":"user","message":"hello"}"#],
        );
        assert_eq!(read_claude_child(&plain).parent, None);
    }

    #[test]
    fn a_child_with_no_meta_file_reports_no_name() {
        let scratch = Scratch::new("child-no-meta");
        let paths = write_child(
            scratch.path(),
            "a9a352",
            "",
            &[r#"{"type":"user","message":"hello"}"#],
        );
        let facts = read_claude_child(&paths);
        assert_eq!(facts.label.name, "");
        assert_eq!(facts.label.title, "");
        assert_eq!(facts.label.color, "");
        assert_eq!(facts.parent, None);
    }

    #[test]
    fn a_child_reads_the_branch_and_the_model_off_its_own_transcript() {
        // Measured on Claude Code 2.1.258. A child's assistant records carry
        // gitBranch, the model and the usage, and sessionId names the lead.
        let scratch = Scratch::new("child-status");
        let paths = write_child(
            scratch.path(),
            "a9a352",
            r#"{"agentType":"Explore","description":"a look"}"#,
            &[concat!(
                r#"{"type":"assistant","sessionId":"4630d1cb","agentId":"a9a352","#,
                r#""gitBranch":"main","message":{"model":"claude-opus-5","#,
                r#""usage":{"input_tokens":2,"output_tokens":1}}}"#
            )],
        );
        let facts = read_claude_child(&paths);
        assert_eq!(facts.status.branch, "main");
        assert_eq!(facts.status.model, "claude-opus-5");
        assert_eq!(facts.status.context_tokens, 3);
    }

    #[test]
    fn a_child_whose_transcript_is_not_there_still_takes_its_name() {
        let scratch = Scratch::new("child-no-transcript");
        let lead = scratch.path().join("4630d1cb.jsonl");
        std::fs::write(&lead, "").unwrap();
        let paths = claude_child_paths(&lead.to_string_lossy(), "a9a352").unwrap();
        std::fs::create_dir_all(Path::new(&paths.transcript).parent().unwrap()).unwrap();
        std::fs::write(
            &paths.meta_file,
            r#"{"agentType":"Plan","description":"d"}"#,
        )
        .unwrap();
        let facts = read_claude_child(&paths);
        assert_eq!(facts.label.name, "Plan");
        assert_eq!(facts.status, StatusFacts::default());
    }

    #[test]
    fn a_transcript_that_is_not_there_says_nothing() {
        assert_eq!(
            read_claude_session("/no/such/transcript.jsonl"),
            SessionFacts::default()
        );
    }

    #[test]
    fn the_title_claude_wrote_is_read() {
        let scratch = Scratch::new("ai-title");
        let path = write_transcript(
            scratch.path(),
            &[
                r#"{"type":"user","message":"hello"}"#,
                r#"{"type":"ai-title","aiTitle":"porting the sidebar"}"#,
            ],
        );
        assert_eq!(
            read_claude_session(&path).label.title,
            "porting the sidebar"
        );
    }

    #[test]
    fn the_later_of_two_titles_of_a_kind_wins() {
        let scratch = Scratch::new("later-title");
        let path = write_transcript(
            scratch.path(),
            &[
                r#"{"type":"ai-title","aiTitle":"first guess"}"#,
                r#"{"type":"ai-title","aiTitle":"second guess"}"#,
            ],
        );
        assert_eq!(read_claude_session(&path).label.title, "second guess");
    }

    #[test]
    fn a_title_the_user_gave_wins_wherever_it_appears() {
        let scratch = Scratch::new("given-title");
        let path = write_transcript(
            scratch.path(),
            &[
                r#"{"type":"custom-title","customTitle":"the port"}"#,
                r#"{"type":"ai-title","aiTitle":"a guess made after it"}"#,
            ],
        );
        assert_eq!(read_claude_session(&path).label.title, "the port");
    }

    #[test]
    fn a_teammate_is_named_by_the_records_it_writes() {
        let scratch = Scratch::new("teammate");
        let path = write_transcript(
            scratch.path(),
            &[
                // The rename record says what the name became. The records
                // written afterwards are what it is called by.
                r#"{"type":"agent-name","agentName":"scout"}"#,
                r#"{"type":"assistant","agentName":"scout","teamName":"port"}"#,
                r#"{"type":"assistant","agentName":"scout","teamName":"port"}"#,
            ],
        );
        assert_eq!(read_claude_session(&path).label.name, "scout");
    }

    #[test]
    fn the_color_the_session_was_given_is_read() {
        let scratch = Scratch::new("color");
        let path = write_transcript(
            scratch.path(),
            &[
                r#"{"type":"agent-color","agentColor":"blue"}"#,
                r#"{"type":"user","message":"hi"}"#,
                r#"{"type":"agent-color","agentColor":"purple"}"#,
            ],
        );
        // The last one in the window is the one in force, as for a title.
        assert_eq!(read_claude_session(&path).label.color, "purple");
    }

    #[test]
    fn a_session_that_has_run_long_reports_only_what_is_still_in_the_window() {
        // What a session records once, as it begins, scrolls out of the window,
        // and nothing looks for it again. A scan says what it can see now. A
        // scan that says nothing about a color does not say that the color is
        // gone.
        let scratch = Scratch::new("long");
        let mut lines = vec![r#"{"type":"agent-color","agentColor":"red"}"#.to_string()];
        let filler = format!(r#"{{"type":"assistant","text":"{}"}}"#, "x".repeat(500));
        lines.resize(lines.len() + 400, filler);
        lines.push(r#"{"type":"ai-title","aiTitle":"the long one"}"#.to_string());
        let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
        let path = write_transcript(scratch.path(), &borrowed);

        assert!(std::fs::metadata(&path).unwrap().len() > TAIL);
        let facts = read_claude_session(&path);
        assert_eq!(facts.label.title, "the long one");
        assert_eq!(facts.label.color, "");
    }

    #[test]
    fn a_session_given_no_color_reports_none() {
        let scratch = Scratch::new("no-color");
        let path = write_transcript(scratch.path(), &[r#"{"type":"user","message":"hi"}"#]);
        assert_eq!(read_claude_session(&path).label.color, "");
    }

    #[test]
    fn a_session_of_its_own_has_no_teammate_name() {
        let scratch = Scratch::new("top-level");
        let path = write_transcript(scratch.path(), &[r#"{"type":"user","message":"hi"}"#]);
        assert_eq!(read_claude_session(&path).label.name, "");
    }

    /// One record of type `assistant`, as Claude writes it: the branch it ran
    /// on, the model that composed the reply, and what the reply counted
    /// against the window.
    fn assistant_record(branch: &str, model: &str, cached: u64, out: u64) -> String {
        format!(
            r#"{{"type":"assistant","gitBranch":"{branch}","message":{{"model":"{model}","usage":{{"input_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":{cached},"output_tokens":{out}}}}}}}"#
        )
    }

    #[test]
    fn what_a_session_works_with_comes_off_the_last_assistant_record() {
        let scratch = Scratch::new("assistant-record");
        let lines = [
            assistant_record("old-branch", "claude-haiku-4-5", 10, 1),
            r#"{"type":"user","message":"and again"}"#.to_string(),
            assistant_record("main", "claude-opus-5", 162_227, 6_540),
        ];
        let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
        let path = write_transcript(scratch.path(), &borrowed);
        assert_eq!(
            read_claude_session(&path).status,
            StatusFacts {
                branch: "main".to_string(),
                model: "claude-opus-5".to_string(),
                context_tokens: 168_769,
            }
        );
    }

    #[test]
    fn a_window_with_no_assistant_record_reports_nothing_worked_with() {
        // The same report as a session that has replied nothing yet.
        let scratch = Scratch::new("no-assistant-record");
        let path = write_transcript(scratch.path(), &[r#"{"type":"user","message":"hi"}"#]);
        assert_eq!(read_claude_session(&path).status, StatusFacts::default());
    }

    #[test]
    fn a_message_that_no_model_composed_says_nothing() {
        // Claude writes such a message for itself when a request fails. It
        // names no model that the user ran, so the record before it stands.
        let scratch = Scratch::new("synthetic");
        let lines = [
            assistant_record("main", "claude-opus-5", 100, 10),
            assistant_record("main", "<synthetic>", 0, 0),
        ];
        let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
        let path = write_transcript(scratch.path(), &borrowed);
        assert_eq!(read_claude_session(&path).status.model, "claude-opus-5");
        assert_eq!(read_claude_session(&path).status.context_tokens, 112);
    }

    #[test]
    fn a_reply_that_counted_nothing_spends_nothing() {
        let scratch = Scratch::new("uncounted");
        let path = write_transcript(
            scratch.path(),
            &[r#"{"type":"assistant","message":{"model":"claude-opus-5"}}"#],
        );
        let status = read_claude_session(&path).status;
        assert_eq!(status.model, "claude-opus-5");
        assert_eq!(status.context_tokens, 0);
        assert_eq!(status.branch, "");
    }

    #[test]
    fn a_line_that_is_not_a_record_is_passed_over() {
        let scratch = Scratch::new("torn");
        let path = write_transcript(
            scratch.path(),
            &[
                r#"{"type":"ai-title","aiTit"#,
                r#"{"type":"ai-title","aiTitle":"whole"}"#,
            ],
        );
        assert_eq!(read_claude_session(&path).label.title, "whole");
    }

    /// One `assistant` record with one text block, as Claude writes it.
    fn text_record(words: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"model":"claude-opus-5","content":[{{"type":"text","text":"{words}"}}]}}}}"#
        )
    }

    /// One `assistant` record with one tool call.
    fn tool_call_record(id: &str, tool: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"model":"claude-opus-5","content":[{{"type":"tool_use","id":"{id}","name":"{tool}","input":{{}}}}]}}}}"#
        )
    }

    /// One `user` record with the result of one tool call.
    fn tool_result_record(id: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{id}","content":"done"}}]}}}}"#
        )
    }

    /// One `assistant` record with a thinking block. Claude writes the signature
    /// of its reasoning, and never the reasoning itself.
    fn thinking_record() -> String {
        r#"{"type":"assistant","message":{"model":"claude-opus-5","content":[{"type":"thinking","thinking":"","signature":"CAISnw8Kpg"}]}}"#
            .to_string()
    }

    /// The record that Claude composes for itself after a request fails.
    fn synthetic_text_record(words: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"model":"<synthetic>","content":[{{"type":"text","text":"{words}"}}]}}}}"#
        )
    }

    /// The two records that a scan of these transcript lines reports.
    fn records_from_lines(dir: &Path, lines: &[String]) -> TranscriptRecords {
        let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
        read_claude_session(&write_transcript(dir, &borrowed)).records
    }

    #[test]
    fn the_most_recent_record_that_says_something_is_the_last_message() {
        // The record travels as it was read. Nothing here extracts the words.
        let scratch = Scratch::new("last-message");
        let records = records_from_lines(
            scratch.path(),
            &[
                text_record("an older answer"),
                tool_call_record("toolu_1", "Bash"),
                text_record("the port is done"),
                tool_call_record("toolu_2", "Grep"),
            ],
        );
        assert_eq!(records.last_message, text_record("the port is done"));
    }

    #[test]
    fn a_record_that_claude_composed_for_itself_is_the_last_message() {
        // The status read passes over this record, because it names no model
        // that the user ran. It is still the last thing the agent said, and it
        // is the only thing that says why the agent stopped.
        let scratch = Scratch::new("composed");
        let stopped = synthetic_text_record("You have hit your session limit");
        let records = records_from_lines(
            scratch.path(),
            &[text_record("working on it"), stopped.clone()],
        );
        assert_eq!(records.last_message, stopped);
    }

    #[test]
    fn a_record_that_holds_only_a_tool_call_is_not_the_last_message() {
        let scratch = Scratch::new("only-a-call");
        let records = records_from_lines(
            scratch.path(),
            &[
                text_record("about to look"),
                tool_call_record("toolu_1", "Bash"),
            ],
        );
        assert_eq!(records.last_message, text_record("about to look"));
    }

    #[test]
    fn a_record_that_holds_only_a_thinking_block_is_not_the_last_message() {
        let scratch = Scratch::new("only-thinking");
        let records = records_from_lines(
            scratch.path(),
            &[text_record("here goes"), thinking_record()],
        );
        assert_eq!(records.last_message, text_record("here goes"));
    }

    #[test]
    fn a_window_with_nothing_said_in_it_reports_no_message() {
        let scratch = Scratch::new("nothing-said");
        let records = records_from_lines(
            scratch.path(),
            &[thinking_record(), tool_call_record("toolu_1", "Bash")],
        );
        assert_eq!(records.last_message, "");
    }

    #[test]
    fn the_tool_call_that_no_result_answered_is_the_tool_that_runs() {
        let scratch = Scratch::new("running-tool");
        let records = records_from_lines(
            scratch.path(),
            &[
                tool_call_record("toolu_1", "Bash"),
                tool_result_record("toolu_1"),
                tool_call_record("toolu_2", "Grep"),
            ],
        );
        assert_eq!(records.running_tool, tool_call_record("toolu_2", "Grep"));
    }

    #[test]
    fn a_tool_call_whose_result_came_back_is_not_reported() {
        let scratch = Scratch::new("finished-tool");
        let records = records_from_lines(
            scratch.path(),
            &[
                tool_call_record("toolu_1", "Bash"),
                tool_result_record("toolu_1"),
            ],
        );
        assert_eq!(records.running_tool, "");
    }

    #[test]
    fn two_tools_that_start_together_report_the_one_that_is_left() {
        let scratch = Scratch::new("pair-in-order");
        let records = records_from_lines(
            scratch.path(),
            &[
                tool_call_record("toolu_1", "Bash"),
                tool_call_record("toolu_2", "Grep"),
                tool_result_record("toolu_1"),
            ],
        );
        assert_eq!(records.running_tool, tool_call_record("toolu_2", "Grep"));
    }

    #[test]
    fn two_tools_whose_results_come_back_in_the_other_order_report_the_one_that_is_left() {
        // This is the case that the pairing exists for. A rule that dropped the
        // oldest call on any result would name Grep here, which finished.
        let scratch = Scratch::new("pair-out-of-order");
        let records = records_from_lines(
            scratch.path(),
            &[
                tool_call_record("toolu_1", "Bash"),
                tool_call_record("toolu_2", "Grep"),
                tool_result_record("toolu_2"),
            ],
        );
        assert_eq!(records.running_tool, tool_call_record("toolu_1", "Bash"));
    }

    #[test]
    fn a_result_for_a_tool_call_outside_the_window_costs_nothing() {
        // The window opens partway through a turn, so the call that this result
        // answers is not in it. The result names nothing here and is dropped.
        let scratch = Scratch::new("orphan-result");
        let records = records_from_lines(
            scratch.path(),
            &[
                tool_result_record("toolu_0"),
                tool_call_record("toolu_1", "Bash"),
            ],
        );
        assert_eq!(records.running_tool, tool_call_record("toolu_1", "Bash"));
    }

    #[test]
    fn a_tool_call_with_no_id_is_not_reported() {
        // Nothing can pair such a call with its result, so it would be reported
        // as running for the rest of the session.
        let scratch = Scratch::new("call-with-no-id");
        let path = write_transcript(
            scratch.path(),
            &[r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#],
        );
        assert_eq!(read_claude_session(&path).records.running_tool, "");
    }

    #[test]
    fn a_session_that_has_said_nothing_and_runs_nothing_reports_neither() {
        let scratch = Scratch::new("no-records");
        let path = write_transcript(scratch.path(), &[r#"{"type":"user","message":"hi"}"#]);
        assert_eq!(
            read_claude_session(&path).records,
            TranscriptRecords::default()
        );
    }

    fn write_workspace(home: &Path, session: &str, text: &str) {
        let path = workspace_path(home, session);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_copilot_session_is_called_by_its_name() {
        let scratch = Scratch::new("copilot-name");
        write_workspace(
            scratch.path(),
            "abc",
            "name: the port\nsummary: something\n",
        );
        assert_eq!(
            read_copilot_session(scratch.path(), "abc").label.title,
            "the port"
        );
    }

    #[test]
    fn a_copilot_session_with_no_name_falls_back_to_its_summary() {
        let scratch = Scratch::new("copilot-summary");
        write_workspace(scratch.path(), "abc", "name: ~\nsummary: 'what it did'\n");
        assert_eq!(
            read_copilot_session(scratch.path(), "abc").label.title,
            "what it did"
        );
    }

    #[test]
    fn a_block_of_text_is_read_as_its_first_line() {
        let scratch = Scratch::new("copilot-block");
        write_workspace(
            scratch.path(),
            "abc",
            "summary: |\n  the first line\n  the second\nname:\n",
        );
        assert_eq!(
            read_copilot_session(scratch.path(), "abc").label.title,
            "the first line"
        );
    }

    #[test]
    fn a_copilot_session_reports_no_transcript_records() {
        // The daemon never opens a Copilot transcript, so there is nothing to
        // read the records out of, and nothing here guesses at them.
        let scratch = Scratch::new("copilot-records");
        write_workspace(scratch.path(), "abc", "name: the port\n");
        assert_eq!(
            read_copilot_session(scratch.path(), "abc").records,
            TranscriptRecords::default()
        );
    }

    #[test]
    fn a_workspace_that_is_not_there_says_nothing() {
        let scratch = Scratch::new("copilot-missing");
        assert_eq!(
            read_copilot_session(scratch.path(), "abc"),
            SessionFacts::default()
        );
    }

    #[test]
    fn a_quoted_scalar_comes_back_as_its_text() {
        assert_eq!(yaml_scalar(r#""a \"quoted\" name""#), r#"a "quoted" name"#);
        assert_eq!(yaml_scalar("'it''s'"), "it's");
        assert_eq!(yaml_scalar("null"), "");
        assert_eq!(yaml_scalar("  plain  "), "plain");
    }
}
