//! This module keeps the state between one run of the daemon and the next.
//!
//! This module writes the file whole and then moves it into place. A reader sees
//! either the previous state or the new state, and never half of either one.
//! Nothing reports a file that this module cannot write. The daemon carries on
//! with the state in memory, which is the state that matters.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use agent_wrangler_core::notify::Notifier;

use crate::daemon::state::{Client, Source};
use crate::proto::Sink;

/// The number of writes that this process made. No two of those writes name the
/// same temporary file.
static WRITES: AtomicU64 = AtomicU64::new(0);

/// One session as this module keeps it. The session holds the record exactly
/// as it goes over the wire. The session also holds the place that the account
/// of the session comes from.
///
/// This module keeps the record as its encoded line, and not as fields. The
/// saved form and the sent form cannot drift apart.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Saved {
    record: String,
    #[serde(default)]
    source: Source,
}

/// One client as this module keeps it: where to reach the client, and what the
/// client asked to announce a call with.
///
/// This module keeps the notifier as the words that it runs, and not as the
/// notifier itself. The stored form is text either way, and words are what the
/// client sent first.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Listening {
    sink: Sink,
    #[serde(default)]
    notify: Vec<String>,
}

/// Everything that this module keeps between one run and the next.
///
/// This module keeps the clients as well as the sessions. A client registers
/// once, and it cannot know that the daemon restarted since then. A daemon that
/// forgets the clients leaves every sidebar with the state that it last
/// received, for good, and tells nobody why.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Kept {
    #[serde(default)]
    sessions: Vec<Saved>,
    #[serde(default)]
    clients: Vec<Listening>,
}

fn file(dir: &Path) -> PathBuf {
    dir.join("agents.json")
}

/// Writes every session out, and replaces whatever was there.
///
/// Side effect: if the state directory is not there, this function creates it.
/// This function writes a temporary file beside the real file, and then renames
/// the temporary file over the real one.
pub fn save(dir: &Path, sessions: &[(String, Source)], clients: &[Client]) {
    let kept = Kept {
        sessions: sessions
            .iter()
            .map(|(record, source)| Saved {
                record: record.clone(),
                source: source.clone(),
            })
            .collect(),
        clients: clients
            .iter()
            .map(|client| Listening {
                sink: client.sink.clone(),
                notify: client
                    .notify
                    .as_ref()
                    .map(Notifier::program_and_arguments)
                    .unwrap_or_default(),
            })
            .collect(),
    };
    let Ok(text) = serde_json::to_string(&kept) else {
        return;
    };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    // The name of the temporary file belongs to this write and to no other
    // write. Two saves that run at once otherwise open, truncate and rename the
    // same path. One save then writes through its own descriptor into the file
    // that the other save already put in place. The result is a torn document,
    // or an empty one, and it reads back as the end of every session.
    let temp = file(dir).with_extension(format!(
        "json.{}.{}",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ));
    let written = File::create(&temp).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        // If the machine stops here, a rename of an unflushed file renames
        // nothing in particular.
        file.sync_all()
    });
    match written {
        Ok(()) => {
            let _ = fs::rename(&temp, file(dir));
        }
        Err(_) => {
            let _ = fs::remove_file(&temp);
        }
    }
}

/// Reads back what `save` wrote. This function reads nothing at all for a first
/// run, for an unreadable file, or for a file that something else wrote.
pub fn load(dir: &Path) -> (Vec<(String, Source)>, Vec<Client>) {
    let Ok(text) = fs::read_to_string(file(dir)) else {
        return (Vec::new(), Vec::new());
    };
    let kept: Kept = serde_json::from_str(&text).unwrap_or_default();
    (
        kept.sessions
            .into_iter()
            .map(|saved| (saved.record, saved.source))
            .collect(),
        kept.clients
            .into_iter()
            .map(|listening| Client {
                sink: listening.sink,
                notify: Notifier::new(listening.notify),
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    #[cfg(unix)]
    use std::sync::Arc;

    /// Enough records that one write is several calls into the kernel. The tear
    /// happens between those calls.
    #[cfg(unix)]
    const RECORDS: usize = 400;

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
        let clients = vec![
            Client {
                sink: Sink::Zellij {
                    session: "proto".to_string(),
                },
                notify: Notifier::new(vec!["notify-send".to_string(), "-u".to_string()]),
            },
            Client {
                sink: Sink::Socket {
                    name: "wrangler-tmux-work.sock".to_string(),
                },
                notify: None,
            },
        ];
        save(&dir, &sessions, &clients);
        assert_eq!(load(&dir), (sessions, clients));
        let _ = fs::remove_dir_all(&dir);
    }

    // A rename over a file that another thread holds open works on unix, and
    // Windows can refuse it. A run on Windows measures that refusal rather than
    // the tear. The fix under test is not unix-only. The method is unix-only.
    #[cfg(unix)]
    #[test]
    fn saving_at_the_same_moment_from_two_threads_leaves_one_whole_file() {
        // Two saves can name one temporary file. One save then writes through
        // its own descriptor into the file that the other save already put into
        // place.
        //
        // This test needs two things to bite. First, each writer writes a
        // document of its own, because writers of identical bytes tear into
        // something that still reads back correctly. Second, a reader runs
        // beside the writers. A tear exists only between one save and the
        // next, and the last writer always leaves a whole file.
        let dir = scratch("concurrent");
        let torn = Arc::new(AtomicUsize::new(0));
        let done = Arc::new(AtomicBool::new(false));

        let watcher = {
            let dir = dir.clone();
            let torn = Arc::clone(&torn);
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    let Ok(text) = fs::read_to_string(file(&dir)) else {
                        continue;
                    };
                    let whole = serde_json::from_str::<Kept>(&text)
                        .map(|kept| kept.sessions.len() == RECORDS)
                        .unwrap_or(false);
                    if !whole {
                        torn.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        };

        let writers: Vec<std::thread::JoinHandle<()>> = (0..8u32)
            .map(|writer| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    let padding = writer.to_string().repeat(2048);
                    let sessions: Vec<(String, Source)> = (0..RECORDS)
                        .map(|n| {
                            (
                                format!("3\ts{n}\tclaude\t\tidle\t0\tdir\t\t\t\t{padding}"),
                                source("/t/x"),
                            )
                        })
                        .collect();
                    for _ in 0..60 {
                        save(&dir, &sessions, &[]);
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        done.store(true, Ordering::Relaxed);
        watcher.join().unwrap();

        assert_eq!(
            torn.load(Ordering::Relaxed),
            0,
            "the state file was readable but not whole while it was being written"
        );
        assert_eq!(load(&dir).0.len(), RECORDS, "what was left is not whole");
        let leftovers = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name() != "agents.json")
            .count();
        assert_eq!(leftovers, 0, "a temporary file was left behind");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_first_run_reads_nothing_rather_than_failing() {
        assert_eq!(load(&scratch("empty")), (Vec::new(), Vec::new()));
    }

    #[test]
    fn a_file_written_by_something_else_reads_as_nothing() {
        let dir = scratch("foreign");
        fs::create_dir_all(&dir).unwrap();
        fs::write(file(&dir), "not json at all").unwrap();
        assert_eq!(load(&dir), (Vec::new(), Vec::new()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_again_replaces_rather_than_adds() {
        let dir = scratch("replace");
        save(
            &dir,
            &[("3\tone\tclaude".to_string(), source("/t/one.jsonl"))],
            &[],
        );
        save(
            &dir,
            &[("3\ttwo\tclaude".to_string(), source("/t/two.jsonl"))],
            &[],
        );
        let (sessions, _) = load(&dir);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].0, "3\ttwo\tclaude");
        let _ = fs::remove_dir_all(&dir);
    }
}
