//! What a session calls itself, read from the files that the agent keeps.
//!
//! Neither agent puts its title in the hook body, so both titles are read from
//! disk at the moment when a hook fires. That is also what keeps a label
//! current. An agent fires hooks throughout its turn, and each hook is another
//! look at a title that can differ from the last one.
//!
//! Nothing here fails. A file that is missing, unreadable or unexpected gives an
//! empty title. That is the same answer as a session with no title yet.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::agent::{LabelFacts, StatusFacts};

/// Everything that an agent's own files say about one session: what it is
/// called by, and what it works with.
///
/// The two travel together because one read finds both. A caller that wants
/// only a label still pays for one read and not two.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionFacts {
    pub label: LabelFacts,
    pub status: StatusFacts,
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
fn tail(path: &Path) -> Option<(Vec<u8>, bool)> {
    let mut file = std::fs::File::open(path).ok()?;
    let size = file.seek(SeekFrom::End(0)).ok()?;
    file.seek(SeekFrom::Start(size.saturating_sub(TAIL))).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some((bytes, size > TAIL))
}

/// A record's string field, or `None` when it is absent or empty.
fn text<'a>(record: &'a Value, key: &str) -> Option<&'a str> {
    record
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
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
    let model = text(message, "model")?;
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
        branch: text(record, "gitBranch").unwrap_or_default().to_string(),
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
pub fn claude(transcript: &str) -> SessionFacts {
    let Some((bytes, cut)) = tail(Path::new(transcript)) else {
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
        ]
        .iter()
        .any(|marker| find(line, marker));
        if !wanted {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        match text(&record, "type") {
            Some("custom-title") => given = text(&record, "customTitle").unwrap_or("").to_string(),
            Some("ai-title") => written = text(&record, "aiTitle").unwrap_or("").to_string(),
            Some("agent-color") => color = text(&record, "agentColor").unwrap_or("").to_string(),
            Some("agent-name") => {}
            Some("assistant") => {
                if name.is_empty() {
                    name = text(&record, "agentName").unwrap_or("").to_string();
                }
                // The later record wins, the way the later title does.
                if let Some(found) = status_from_assistant_record(&record) {
                    status = found;
                }
            }
            _ if name.is_empty() => name = text(&record, "agentName").unwrap_or("").to_string(),
            _ => {}
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
    }
}

/// Whether `haystack` holds `needle`.
fn find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Where Copilot keeps what it knows about one session.
fn workspace(home: &Path, session: &str) -> PathBuf {
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
/// of a Copilot session is empty.
pub fn copilot(home: &Path, session: &str) -> SessionFacts {
    let Ok(text) = std::fs::read_to_string(workspace(home, session)) else {
        return SessionFacts::default();
    };
    let title = match field(&text, "name") {
        title if title.is_empty() => field(&text, "summary"),
        title => title,
    };
    SessionFacts {
        label: LabelFacts {
            title,
            ..LabelFacts::default()
        },
        status: StatusFacts::default(),
    }
}

/// One top-level field of a workspace file, reduced to a single line.
///
/// The reader undoes only the quotes that a written-out YAML scalar uses. A
/// value written as a block is taken as its first non-empty line. A title is one
/// line of text, and the rest of a block is not part of it.
fn field(text: &str, key: &str) -> String {
    let prefix = format!("{key}:");
    let mut lines = text.lines();
    let Some(first) = lines.find_map(|line| line.strip_prefix(&prefix)) else {
        return String::new();
    };
    let first = first.trim();
    if !matches!(first, "|" | "|-" | "|+" | ">" | ">-" | ">+") {
        return scalar(first);
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
fn scalar(value: &str) -> String {
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

    fn transcript(dir: &Path, lines: &[&str]) -> String {
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

    #[test]
    fn a_transcript_that_is_not_there_says_nothing() {
        assert_eq!(claude("/no/such/transcript.jsonl"), SessionFacts::default());
    }

    #[test]
    fn the_title_claude_wrote_is_read() {
        let scratch = Scratch::new("ai-title");
        let path = transcript(
            scratch.path(),
            &[
                r#"{"type":"user","message":"hello"}"#,
                r#"{"type":"ai-title","aiTitle":"porting the sidebar"}"#,
            ],
        );
        assert_eq!(claude(&path).label.title, "porting the sidebar");
    }

    #[test]
    fn the_later_of_two_titles_of_a_kind_wins() {
        let scratch = Scratch::new("later-title");
        let path = transcript(
            scratch.path(),
            &[
                r#"{"type":"ai-title","aiTitle":"first guess"}"#,
                r#"{"type":"ai-title","aiTitle":"second guess"}"#,
            ],
        );
        assert_eq!(claude(&path).label.title, "second guess");
    }

    #[test]
    fn a_title_the_user_gave_wins_wherever_it_appears() {
        let scratch = Scratch::new("given-title");
        let path = transcript(
            scratch.path(),
            &[
                r#"{"type":"custom-title","customTitle":"the port"}"#,
                r#"{"type":"ai-title","aiTitle":"a guess made after it"}"#,
            ],
        );
        assert_eq!(claude(&path).label.title, "the port");
    }

    #[test]
    fn a_teammate_is_named_by_the_records_it_writes() {
        let scratch = Scratch::new("teammate");
        let path = transcript(
            scratch.path(),
            &[
                // The rename record says what the name became. The records
                // written afterwards are what it is called by.
                r#"{"type":"agent-name","agentName":"scout"}"#,
                r#"{"type":"assistant","agentName":"scout","teamName":"port"}"#,
                r#"{"type":"assistant","agentName":"scout","teamName":"port"}"#,
            ],
        );
        assert_eq!(claude(&path).label.name, "scout");
    }

    #[test]
    fn the_color_the_session_was_given_is_read() {
        let scratch = Scratch::new("color");
        let path = transcript(
            scratch.path(),
            &[
                r#"{"type":"agent-color","agentColor":"blue"}"#,
                r#"{"type":"user","message":"hi"}"#,
                r#"{"type":"agent-color","agentColor":"purple"}"#,
            ],
        );
        // The last one in the window is the one in force, as for a title.
        assert_eq!(claude(&path).label.color, "purple");
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
        let path = transcript(scratch.path(), &borrowed);

        assert!(std::fs::metadata(&path).unwrap().len() > TAIL);
        let facts = claude(&path);
        assert_eq!(facts.label.title, "the long one");
        assert_eq!(facts.label.color, "");
    }

    #[test]
    fn a_session_given_no_color_reports_none() {
        let scratch = Scratch::new("no-color");
        let path = transcript(scratch.path(), &[r#"{"type":"user","message":"hi"}"#]);
        assert_eq!(claude(&path).label.color, "");
    }

    #[test]
    fn a_session_of_its_own_has_no_teammate_name() {
        let scratch = Scratch::new("top-level");
        let path = transcript(scratch.path(), &[r#"{"type":"user","message":"hi"}"#]);
        assert_eq!(claude(&path).label.name, "");
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
        let path = transcript(scratch.path(), &borrowed);
        assert_eq!(
            claude(&path).status,
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
        let path = transcript(scratch.path(), &[r#"{"type":"user","message":"hi"}"#]);
        assert_eq!(claude(&path).status, StatusFacts::default());
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
        let path = transcript(scratch.path(), &borrowed);
        assert_eq!(claude(&path).status.model, "claude-opus-5");
        assert_eq!(claude(&path).status.context_tokens, 112);
    }

    #[test]
    fn a_reply_that_counted_nothing_spends_nothing() {
        let scratch = Scratch::new("uncounted");
        let path = transcript(
            scratch.path(),
            &[r#"{"type":"assistant","message":{"model":"claude-opus-5"}}"#],
        );
        let status = claude(&path).status;
        assert_eq!(status.model, "claude-opus-5");
        assert_eq!(status.context_tokens, 0);
        assert_eq!(status.branch, "");
    }

    #[test]
    fn a_line_that_is_not_a_record_is_passed_over() {
        let scratch = Scratch::new("torn");
        let path = transcript(
            scratch.path(),
            &[
                r#"{"type":"ai-title","aiTit"#,
                r#"{"type":"ai-title","aiTitle":"whole"}"#,
            ],
        );
        assert_eq!(claude(&path).label.title, "whole");
    }

    fn workspace_at(home: &Path, session: &str, text: &str) {
        let path = workspace(home, session);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_copilot_session_is_called_by_its_name() {
        let scratch = Scratch::new("copilot-name");
        workspace_at(
            scratch.path(),
            "abc",
            "name: the port\nsummary: something\n",
        );
        assert_eq!(copilot(scratch.path(), "abc").label.title, "the port");
    }

    #[test]
    fn a_copilot_session_with_no_name_falls_back_to_its_summary() {
        let scratch = Scratch::new("copilot-summary");
        workspace_at(scratch.path(), "abc", "name: ~\nsummary: 'what it did'\n");
        assert_eq!(copilot(scratch.path(), "abc").label.title, "what it did");
    }

    #[test]
    fn a_block_of_text_is_read_as_its_first_line() {
        let scratch = Scratch::new("copilot-block");
        workspace_at(
            scratch.path(),
            "abc",
            "summary: |\n  the first line\n  the second\nname:\n",
        );
        assert_eq!(copilot(scratch.path(), "abc").label.title, "the first line");
    }

    #[test]
    fn a_workspace_that_is_not_there_says_nothing() {
        let scratch = Scratch::new("copilot-missing");
        assert_eq!(copilot(scratch.path(), "abc"), SessionFacts::default());
    }

    #[test]
    fn a_quoted_scalar_comes_back_as_its_text() {
        assert_eq!(scalar(r#""a \"quoted\" name""#), r#"a "quoted" name"#);
        assert_eq!(scalar("'it''s'"), "it's");
        assert_eq!(scalar("null"), "");
        assert_eq!(scalar("  plain  "), "plain");
    }
}
