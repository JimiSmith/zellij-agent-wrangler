//! Keeping what is held between one run of the daemon and the next.
//!
//! The file is written whole and moved into place, so a reader either sees the
//! previous state or the new one and never half of either. A file that cannot be
//! written is not reported anywhere: the daemon carries on with the state in
//! memory, which is the state that matters.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use agent_wrangler_core::notify::Notifier;

use crate::daemon::state::{Client, Source};
use crate::proto::Sink;

/// Counts the writes this process has made, so no two of them name the same
/// temporary file.
static WRITES: AtomicU64 = AtomicU64::new(0);

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

/// One client as it is kept: where to reach it, and what it asked to have a
/// call announced with.
///
/// The notifier is kept as the words it runs rather than as the notifier
/// itself, since what is stored is text either way and words are what a client
/// said in the first place.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Listening {
    sink: Sink,
    #[serde(default)]
    notify: Vec<String>,
}

/// Everything kept between one run and the next.
///
/// The clients are kept as well as the sessions, because a client registers
/// once and has no way of knowing it is talking to a daemon that has restarted
/// since. Forgetting them would leave every sidebar drawing whatever it last
/// received, for good, with nothing said about why.
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

/// Write every session out, replacing whatever was there.
///
/// Side effect: creates the state directory if it is not there, writes a
/// temporary file beside the real one and renames over it.
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
                    .map(Notifier::words)
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
    // The temporary is named for this write and no other. Two saves running at
    // once otherwise open, truncate and rename the same path, which ends with
    // one of them writing through its own descriptor into the file the other
    // has already put in place: a torn document, or an empty one, which reads
    // back as every session having ended.
    let temp = file(dir).with_extension(format!(
        "json.{}.{}",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ));
    let written = File::create(&temp).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        // Renaming an unflushed file is a rename of nothing in particular if
        // the machine stops here.
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

/// Read back what `save` wrote. Nothing at all for a first run, an unreadable
/// file, or one written by something else.
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

    /// Enough records that one write is several calls into the kernel, because
    /// the tear happens between them.
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
                sink: Sink::Pipe {
                    path: "/tmp/w.pipe".to_string(),
                },
                notify: None,
            },
        ];
        save(&dir, &sessions, &clients);
        assert_eq!(load(&dir), (sessions, clients));
        let _ = fs::remove_dir_all(&dir);
    }

    // Renaming over a file another thread holds open succeeds on unix and can
    // be refused on Windows, so a run there would be measuring that rather than
    // the tear this is about. The fix it checks is not unix-only; the method is.
    #[cfg(unix)]
    #[test]
    fn saving_at_the_same_moment_from_two_threads_leaves_one_whole_file() {
        // Two saves naming one temporary file end with one of them writing
        // through its own descriptor into the file the other has already put
        // into place.
        //
        // Two things this test needs to bite. Each writer writes a document of
        // its own, because writers producing identical bytes tear into
        // something that still reads back correctly; and a reader runs
        // alongside them, because a tear exists only between one save and the
        // next, and whatever the last writer leaves is always whole.
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
