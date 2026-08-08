//! What a session calls itself, read from the files the agent running it keeps.
//!
//! Neither agent puts its title in the hook body, so both are read from disk at
//! the moment a hook fires. That is also what keeps a label current: an agent
//! fires hooks throughout its turn, and each one is another look at a title
//! that may have changed since the last.
//!
//! Nothing here fails: a file that is missing, unreadable or not what was
//! expected yields an empty title, which is the same answer as a session that
//! has not titled itself yet.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::agents::Meta;

/// How much of a transcript's end is read. The records naming a session sit
/// within a few KB of the end in practice, and reading a fixed window keeps the
/// cost of a hook the same however long the session has run.
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

/// What a Claude session is called, read from its transcript.
///
/// Two kinds of title are recorded: the one the user gave it and the one Claude
/// wrote for itself. A given name wins wherever both appear, since it was asked
/// for; between two of a kind the later one wins, since it is the later name.
///
/// The color is read the same way as a title Claude wrote for itself: the last
/// one in the window is the one in force.
///
/// A teammate's own name rides on every conversation record it writes, so the
/// first one found in the window answers. The record that *renames* a teammate
/// carries the same field and is passed over: it says what the name became, and
/// the conversation records after it already say so.
pub fn claude(transcript: &str) -> Meta {
    let Some((bytes, cut)) = tail(Path::new(transcript)) else {
        return Meta::default();
    };
    let mut lines = bytes.split(|byte| *byte == b'\n');
    // The first line of a window that starts mid-file is half a record.
    if cut {
        lines.next();
    }

    let (mut given, mut written) = (String::new(), String::new());
    let (mut name, mut color) = (String::new(), String::new());
    for line in lines {
        // Reading every record as JSON would parse the whole conversation; the
        // records worth reading name themselves in bytes that can be looked for
        // first.
        let wanted = [
            b"\"custom-title\"".as_slice(),
            b"\"ai-title\"",
            b"\"agent-color\"",
            b"\"agentName\"",
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
            _ if name.is_empty() => name = text(&record, "agentName").unwrap_or("").to_string(),
            _ => {}
        }
    }

    Meta {
        dir: String::new(),
        name,
        color,
        title: if given.is_empty() { written } else { given },
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

/// What a Copilot session is called: the name it was given, or the summary it
/// wrote for itself.
///
/// Copilot has no teammates of its own, so a session read here is always one of
/// its own.
pub fn copilot(home: &Path, session: &str) -> Meta {
    let Ok(text) = std::fs::read_to_string(workspace(home, session)) else {
        return Meta::default();
    };
    let title = match field(&text, "name") {
        title if title.is_empty() => field(&text, "summary"),
        title => title,
    };
    Meta {
        title,
        ..Meta::default()
    }
}

/// One top-level field of a workspace file, reduced to a single line.
///
/// Only the quoting a written-out YAML scalar uses is undone, and a value
/// written as a block is taken as its first non-empty line: a title is one line
/// of text, and the rest of a block is not part of it.
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
        // A line starting in the first column has left the block behind.
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
            let path = std::env::temp_dir().join(format!("zellij-wrangler-{name}"));
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
        assert_eq!(claude("/no/such/transcript.jsonl"), Meta::default());
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
        assert_eq!(claude(&path).title, "porting the sidebar");
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
        assert_eq!(claude(&path).title, "second guess");
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
        assert_eq!(claude(&path).title, "the port");
    }

    #[test]
    fn a_teammate_is_named_by_the_records_it_writes() {
        let scratch = Scratch::new("teammate");
        let path = transcript(
            scratch.path(),
            &[
                // The rename record says what the name became; the records
                // written afterwards are what it is called by.
                r#"{"type":"agent-name","agentName":"scout"}"#,
                r#"{"type":"assistant","agentName":"scout","teamName":"port"}"#,
                r#"{"type":"assistant","agentName":"scout","teamName":"port"}"#,
            ],
        );
        assert_eq!(claude(&path).name, "scout");
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
        assert_eq!(claude(&path).color, "purple");
    }

    #[test]
    fn a_session_given_no_color_reports_none() {
        let scratch = Scratch::new("no-color");
        let path = transcript(scratch.path(), &[r#"{"type":"user","message":"hi"}"#]);
        assert_eq!(claude(&path).color, "");
    }

    #[test]
    fn a_session_of_its_own_has_no_teammate_name() {
        let scratch = Scratch::new("top-level");
        let path = transcript(scratch.path(), &[r#"{"type":"user","message":"hi"}"#]);
        assert_eq!(claude(&path).name, "");
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
        assert_eq!(claude(&path).title, "whole");
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
        assert_eq!(copilot(scratch.path(), "abc").title, "the port");
    }

    #[test]
    fn a_copilot_session_with_no_name_falls_back_to_its_summary() {
        let scratch = Scratch::new("copilot-summary");
        workspace_at(scratch.path(), "abc", "name: ~\nsummary: 'what it did'\n");
        assert_eq!(copilot(scratch.path(), "abc").title, "what it did");
    }

    #[test]
    fn a_block_of_text_is_read_as_its_first_line() {
        let scratch = Scratch::new("copilot-block");
        workspace_at(
            scratch.path(),
            "abc",
            "summary: |\n  the first line\n  the second\nname:\n",
        );
        assert_eq!(copilot(scratch.path(), "abc").title, "the first line");
    }

    #[test]
    fn a_workspace_that_is_not_there_says_nothing() {
        let scratch = Scratch::new("copilot-missing");
        assert_eq!(copilot(scratch.path(), "abc"), Meta::default());
    }

    #[test]
    fn a_quoted_scalar_comes_back_as_its_text() {
        assert_eq!(scalar(r#""a \"quoted\" name""#), r#"a "quoted" name"#);
        assert_eq!(scalar("'it''s'"), "it's");
        assert_eq!(scalar("null"), "");
        assert_eq!(scalar("  plain  "), "plain");
    }
}
