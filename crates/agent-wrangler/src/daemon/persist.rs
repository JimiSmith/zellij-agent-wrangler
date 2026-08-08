//! Keeping what is held between one run of the daemon and the next.
//!
//! The file is written whole and moved into place, so a reader either sees the
//! previous state or the new one and never half of either. A file that cannot be
//! written is not reported anywhere: the daemon carries on with the state in
//! memory, which is the state that matters.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::daemon::state::Source;

/// One session as it is kept: the record exactly as it goes over the wire, and
/// where its own account of itself is read from.
///
/// The record is kept as its encoded line rather than as fields, so what is
/// saved and what is sent cannot drift apart.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Saved {
    record: String,
    #[serde(default)]
    source: Source,
}

fn file(dir: &Path) -> PathBuf {
    dir.join("agents.json")
}

/// Write every session out, replacing whatever was there.
///
/// Side effect: creates the state directory if it is not there, writes a
/// temporary file beside the real one and renames over it.
pub fn save(dir: &Path, sessions: &[(String, Source)]) {
    let saved: Vec<Saved> = sessions
        .iter()
        .map(|(record, source)| Saved {
            record: record.clone(),
            source: source.clone(),
        })
        .collect();
    let Ok(text) = serde_json::to_string(&saved) else {
        return;
    };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let temp = file(dir).with_extension("json.new");
    if fs::write(&temp, text).is_ok() {
        let _ = fs::rename(&temp, file(dir));
    }
}

/// Read back what `save` wrote. Nothing at all for a first run, an unreadable
/// file, or one written by something else.
pub fn load(dir: &Path) -> Vec<(String, Source)> {
    let Ok(text) = fs::read_to_string(file(dir)) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<Saved>>(&text)
        .unwrap_or_default()
        .into_iter()
        .map(|saved| (saved.record, saved.source))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agent-wrangler-persist-{name}"));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn source(transcript: &str) -> Source {
        Source {
            agent: "claude".to_string(),
            transcript: transcript.to_string(),
            mtime: Some(42),
        }
    }

    #[test]
    fn what_is_written_is_what_comes_back() {
        let dir = scratch("round-trip");
        let sessions = vec![
            ("3\tone\tclaude".to_string(), source("/t/one.jsonl")),
            ("3\ttwo\tcopilot".to_string(), source("/t/two.jsonl")),
        ];
        save(&dir, &sessions);
        assert_eq!(load(&dir), sessions);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_first_run_reads_nothing_rather_than_failing() {
        assert!(load(&scratch("empty")).is_empty());
    }

    #[test]
    fn a_file_written_by_something_else_reads_as_nothing() {
        let dir = scratch("foreign");
        fs::create_dir_all(&dir).unwrap();
        fs::write(file(&dir), "not json at all").unwrap();
        assert!(load(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_again_replaces_rather_than_adds() {
        let dir = scratch("replace");
        save(
            &dir,
            &[("3\tone\tclaude".to_string(), source("/t/one.jsonl"))],
        );
        save(
            &dir,
            &[("3\ttwo\tclaude".to_string(), source("/t/two.jsonl"))],
        );
        let read = load(&dir);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].0, "3\ttwo\tclaude");
        let _ = fs::remove_dir_all(&dir);
    }
}
