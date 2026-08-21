//! This module delivers one state change to one client, and reads what a client
//! says back.
//!
//! A sink says how to reach a client, and says nothing about what the client is.
//! A new multiplexer therefore costs one variant and one arm in this module.
//! Nothing else learns about the new multiplexer.
//!
//! A zellij client is reached through one `zellij pipe` that stays open, and not
//! through one process per delivery. A pipe with no payload argument reads its
//! stdin, and one line on that stdin is one message. A plugin answers on the
//! same pipe, and that answer arrives on the stdout of the same process. So one
//! child carries the state out and the messages back.
//!
//! Nothing in this module waits on a delivery. Each held child has a writer of
//! its own, and a slot that holds one payload. A caller fills the slot and
//! returns. A session whose pipe buffer is full therefore delays that session
//! and no other.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::{BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;

use agent_wrangler_core::agent::{flatten, AGENTS_MESSAGE};

use crate::platform::command;
use crate::proto::{read_message, Sink, Told};

/// The outcome of one delivery.
///
/// A client that this module cannot reach is a client that went away. This
/// outcome therefore decides whether the daemon keeps the sink. This type is
/// deliberately not an error type. There is nothing to report, and nobody to
/// report it to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    Failed,
}

/// What a writer waits for, and the payload that it writes next.
///
/// The slot holds one payload rather than a queue. Every delivery carries the
/// whole state, so a payload that a newer one replaced is a payload that nobody
/// needs. A client that reads slowly therefore costs memory that does not grow,
/// and it receives the newest state rather than the oldest.
#[derive(Default)]
struct Slot {
    waiting: Mutex<Waiting>,
    told: Condvar,
}

#[derive(Default, PartialEq, Eq)]
enum Waiting {
    #[default]
    Nothing,
    /// A message with nothing in it, so that the plugins on this pipe get a
    /// turn. A payload replaces one of these. One of these never replaces a
    /// payload.
    Nudge,
    Payload(String),
    /// The child is finished with. A writer that finds this returns.
    Closed,
}

impl Slot {
    /// Puts one payload in, and replaces whatever waits there.
    fn fill(&self, payload: String) {
        let mut waiting = self.held();
        if *waiting == Waiting::Closed {
            return;
        }
        *waiting = Waiting::Payload(payload);
        self.told.notify_one();
    }

    /// Asks for an empty message. If a payload already waits, this method does
    /// nothing.
    ///
    /// A payload in the slot is a state that no client received yet. A nudge
    /// carries no state at all. A nudge that replaced a payload therefore
    /// throws that state away, and no client ever sees it.
    fn nudge(&self) {
        let mut waiting = self.held();
        if *waiting != Waiting::Nothing {
            return;
        }
        *waiting = Waiting::Nudge;
        self.told.notify_one();
    }

    /// Says that there is nothing more to write, and wakes the writer so that
    /// it stops.
    fn close(&self) {
        *self.held() = Waiting::Closed;
        self.told.notify_one();
    }

    /// Waits for a payload and takes it. `None` says that the slot is closed.
    fn take(&self) -> Option<String> {
        let mut waiting = self.held();
        loop {
            match std::mem::take(&mut *waiting) {
                Waiting::Payload(payload) => return Some(payload),
                Waiting::Nudge => return Some(String::new()),
                Waiting::Closed => {
                    *waiting = Waiting::Closed;
                    return None;
                }
                Waiting::Nothing => {
                    waiting = self
                        .told
                        .wait(waiting)
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                }
            }
        }
    }

    fn held(&self) -> MutexGuard<'_, Waiting> {
        self.waiting
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One `zellij pipe` that the daemon holds open, and the slot that feeds it.
///
/// The writer shares the child, so that it can ask whether the process still
/// runs. The reader needs no such handle. The end of the stdout stream tells it
/// the same thing.
struct Held {
    child: Arc<Mutex<Child>>,
    slot: Arc<Slot>,
}

impl Held {
    /// Whether the pipe process still runs. This question does not wait.
    fn alive(&self) -> bool {
        matches!(
            self.child
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .try_wait(),
            Ok(None)
        )
    }
}

/// Every held pipe, keyed by the session that it reaches.
///
/// One thread owns this. A held pipe is a process and two threads, and nothing
/// else in the daemon has a reason to name one.
pub struct Pipes {
    held: BTreeMap<String, Held>,
    /// What the writers report, and the end that a caller drains.
    reported: Sender<(Sink, Delivery)>,
    outcomes: Receiver<(Sink, Delivery)>,
    /// Where a message from a client goes. The readers all share this end.
    told: Sender<Told>,
}

impl Pipes {
    pub fn new(told: Sender<Told>) -> Self {
        let (reported, outcomes) = channel();
        Pipes {
            held: BTreeMap::new(),
            reported,
            outcomes,
            told,
        }
    }

    /// Queues one payload for one zellij session, and opens the pipe if none is
    /// open.
    ///
    /// Side effect: this method can spawn a process. It never waits for one. A
    /// spawn that fails counts as a failed delivery. A session that has gone
    /// answers in exactly that way.
    fn zellij(&mut self, session: &str, payload: &str) {
        // The question costs one call that does not wait, and it is asked here
        // rather than left to the writer. A writer learns that its child died
        // only from the write that failed, which is one publish after this one.
        // The sidebar then misses every state until the publish after that.
        if self.held.get(session).is_some_and(|held| !held.alive()) {
            if let Some(dead) = self.held.remove(session) {
                shut(&dead);
            }
        }
        if !self.held.contains_key(session) {
            let Some(held) = self.open(session) else {
                self.report(
                    &Sink::Zellij {
                        session: session.to_string(),
                    },
                    Delivery::Failed,
                );
                return;
            };
            self.held.insert(session.to_string(), held);
        }
        self.held[session].slot.fill(flatten(payload));
    }

    /// Opens one pipe into one session, with a thread to write to it and a
    /// thread to read from it.
    ///
    /// Side effect: this method spawns a process and two threads. The command
    /// carries no payload argument, which is what makes it read its stdin and
    /// stay open.
    fn open(&self, session: &str) -> Option<Held> {
        let mut piping = command("zellij");
        piping
            .args(["--session", session, "pipe", "--name", AGENTS_MESSAGE])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = piping.spawn().ok()?;
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let child = Arc::new(Mutex::new(child));
        let slot = Arc::new(Slot::default());
        let sink = Sink::Zellij {
            session: session.to_string(),
        };

        let writing = (Arc::clone(&slot), Arc::clone(&child), self.reported.clone());
        thread::spawn(move || {
            let (slot, child, reported) = writing;
            write_until_closed(&sink, stdin, &slot, &child, &reported);
        });
        let told = self.told.clone();
        thread::spawn(move || read_until_ended(stdout, &told));

        Some(Held { child, slot })
    }

    /// Records one outcome that this thread already knows.
    fn report(&self, sink: &Sink, delivery: Delivery) {
        let _ = self.reported.send((sink.clone(), delivery));
    }

    /// Every delivery outcome reported since the last call to this method.
    ///
    /// Side effect: a failed delivery closes the pipe that it was for. A child
    /// that died is therefore replaced by the next delivery to that session,
    /// rather than written to for as long as the daemon runs.
    pub fn outcomes(&mut self) -> Vec<(Sink, Delivery)> {
        let outcomes: Vec<(Sink, Delivery)> = self.outcomes.try_iter().collect();
        for (sink, delivery) in &outcomes {
            if let (Sink::Zellij { session }, Delivery::Failed) = (sink, delivery) {
                if let Some(held) = self.held.remove(session) {
                    shut(&held);
                }
            }
        }
        outcomes
    }

    /// Gives the plugins on every held pipe a message, so that they can speak.
    ///
    /// A plugin writes on a pipe only while it handles a message from that
    /// pipe. Zellij holds anything it writes at any other moment, and hands it
    /// over on the next message. Without this, a sidebar that answered a call
    /// holds that answer until something else changes, and nothing else
    /// changes, because the answer is the change.
    ///
    /// Side effect: this method writes one empty line to each held pipe. The
    /// line is a message to zellij and nothing to a sidebar, which reads no
    /// state in it and draws nothing again.
    ///
    /// This method passes over a pipe whose process ended. Only a delivery
    /// opens a pipe again, because only a delivery knows that the session is
    /// still a client.
    pub fn nudge(&mut self) {
        for held in self.held.values() {
            if held.alive() {
                held.slot.nudge();
            }
        }
    }

    /// Closes every pipe whose session is no longer a client.
    ///
    /// Side effect: this method kills a process and waits for it. A pipe process
    /// does not exit when its session is killed. A daemon that only forgets the
    /// client therefore leaves the process behind, for as long as the user
    /// stays logged in.
    pub fn retain(&mut self, live: &BTreeSet<String>) {
        self.held.retain(|session, held| {
            if live.contains(session) {
                return true;
            }
            shut(held);
            false
        });
    }
}

/// Kills one held pipe, waits for it, and stops its writer.
///
/// The wait matters as much as the kill. A process that receives a signal and is
/// not reaped stays in the table as a zombie. A daemon runs for as long as the
/// user stays logged in.
fn shut(held: &Held) {
    held.slot.close();
    let mut child = held
        .child
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = child.kill();
    let _ = child.wait();
}

/// Writes each payload as one line, until the slot is closed.
///
/// The check after the write is what finds a session that has gone. A pipe into
/// such a session exits within milliseconds. The write before that still lands
/// in the buffer of a process that is on its way out.
fn write_until_closed(
    sink: &Sink,
    mut stdin: ChildStdin,
    slot: &Slot,
    child: &Arc<Mutex<Child>>,
    reported: &Sender<(Sink, Delivery)>,
) {
    while let Some(line) = slot.take() {
        let wrote = writeln!(stdin, "{line}")
            .and_then(|()| stdin.flush())
            .is_ok();
        let gone = !matches!(
            child
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .try_wait(),
            Ok(None)
        );
        let delivery = match wrote && !gone {
            true => Delivery::Sent,
            false => Delivery::Failed,
        };
        if reported.send((sink.clone(), delivery)).is_err() {
            return;
        }
    }
}

/// Reads what the plugins of one session say, until the stream ends.
///
/// The end of the stream is the end of this thread. Nothing else stops it, and
/// nothing needs to. A killed child closes its stdout.
fn read_until_ended(stdout: ChildStdout, told: &Sender<Told>) {
    let mut reader = BufReader::new(stdout);
    while let Ok(Some(message)) = read_message::<_, Told>(&mut reader) {
        if told.send(message).is_err() {
            return;
        }
    }
}

/// Hands one payload to one client.
///
/// Side effect: this function queues a write, or writes to a file. The sink
/// decides which one. Neither one waits for the client to take the payload. The
/// outcome arrives later, on [`Pipes::outcomes`].
pub fn deliver(pipes: &mut Pipes, sink: &Sink, payload: &str) {
    match sink {
        Sink::Zellij { session } => pipes.zellij(session, payload),
        Sink::Pipe { path } => {
            let delivery = match pipe(Path::new(path), payload) {
                true => Delivery::Sent,
                false => Delivery::Failed,
            };
            pipes.report(sink, delivery);
        }
    }
}

/// Appends one line to a named pipe. A named pipe reaches a client that is not
/// a process that this module can run.
///
/// This function writes the payload as a single line. The reader at the other
/// end takes one state per read, whatever the order of the writes.
fn pipe(path: &Path, payload: &str) -> bool {
    OpenOptions::new()
        .append(true)
        .open(path)
        .and_then(|mut file| writeln!(file, "{}", flatten(payload)))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_wrangler_core::agent::BREAK;

    fn pipes() -> Pipes {
        let (told, _heard) = channel();
        Pipes::new(told)
    }

    #[test]
    fn a_pipe_that_is_not_there_is_a_client_that_has_gone() {
        let mut pipes = pipes();
        let sink = Sink::Pipe {
            path: "/nonexistent/agent-wrangler/pipe".to_string(),
        };
        deliver(&mut pipes, &sink, "anything");
        assert_eq!(pipes.outcomes(), vec![(sink, Delivery::Failed)]);
    }

    #[test]
    fn a_run_of_records_reaches_a_pipe_as_one_line() {
        let dir = std::env::temp_dir().join("agent-wrangler-sink-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("one.pipe");
        std::fs::write(&path, "").unwrap();
        let sink = Sink::Pipe {
            path: path.to_string_lossy().to_string(),
        };
        let mut pipes = pipes();
        deliver(&mut pipes, &sink, "first\nsecond");
        assert_eq!(pipes.outcomes(), vec![(sink, Delivery::Sent)]);
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written.lines().count(), 1);
        assert!(written.contains(BREAK), "the record break is kept");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_slot_holds_the_newest_payload_and_not_a_queue() {
        // The whole point of one slot: every delivery carries the whole state,
        // so a payload that a newer one replaced is a payload nobody needs.
        let slot = Slot::default();
        slot.fill("first".to_string());
        slot.fill("second".to_string());
        slot.fill("third".to_string());
        assert_eq!(slot.take(), Some("third".to_string()));
    }

    #[test]
    fn a_nudge_never_stands_in_front_of_a_state() {
        // A payload in the slot is a state that no client received yet. A
        // nudge carries no state at all, so a nudge that replaced a payload
        // throws that state away.
        let slot = Slot::default();
        slot.fill("state".to_string());
        slot.nudge();
        assert_eq!(slot.take(), Some("state".to_string()));
    }

    #[test]
    fn a_nudge_is_a_message_with_nothing_in_it() {
        let slot = Slot::default();
        slot.nudge();
        assert_eq!(slot.take(), Some(String::new()));
    }

    #[test]
    fn a_state_replaces_a_nudge_that_has_not_gone_yet() {
        let slot = Slot::default();
        slot.nudge();
        slot.fill("state".to_string());
        assert_eq!(slot.take(), Some("state".to_string()));
    }

    #[test]
    fn a_closed_slot_takes_nothing_more_and_releases_its_writer() {
        let slot = Arc::new(Slot::default());
        slot.fill("first".to_string());
        slot.close();
        assert_eq!(slot.take(), None, "the writer is released");
        slot.fill("later".to_string());
        assert_eq!(slot.take(), None, "and it stays released");
    }

    // A held child is a process, and the two systems share no spelling of one
    // that exits at once. The rule under test is not unix-only. The program is.
    #[cfg(unix)]
    #[test]
    fn a_pipe_whose_process_ended_is_not_a_pipe_to_write_to() {
        // What a killed pipe looks like. Without this question, a writer learns
        // that its child died only from the write that failed. The sidebar then
        // misses every state until the publish after that one.
        let child = command("true").spawn().expect("a program that exits");
        let held = Held {
            child: Arc::new(Mutex::new(child)),
            slot: Arc::new(Slot::default()),
        };
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while held.alive() && std::time::Instant::now() < until {
            thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!held.alive(), "the process ended, so the pipe is gone");
    }

    #[test]
    fn a_writer_waits_until_there_is_something_to_write() {
        let slot = Arc::new(Slot::default());
        let waiting = Arc::clone(&slot);
        let (took, heard) = channel();
        thread::spawn(move || {
            let _ = took.send(waiting.take());
        });
        assert!(
            heard
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err(),
            "nothing is waiting, so the writer sleeps"
        );
        slot.fill("state".to_string());
        assert_eq!(
            heard.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(Some("state".to_string()))
        );
    }
}
