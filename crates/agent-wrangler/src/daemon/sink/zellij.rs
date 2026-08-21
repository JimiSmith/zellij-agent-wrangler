//! One `zellij pipe` per session, held open for as long as the session is a
//! client.
//!
//! A zellij client is reached through one pipe that stays open, and not through
//! one process per delivery. A pipe with no payload argument reads its stdin,
//! and one line on that stdin is one message. A plugin answers on the same pipe,
//! and that answer arrives on the stdout of the same process. So one child
//! carries the state out and the messages back.

use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

use agent_wrangler_core::agent::AGENTS_MESSAGE;

use super::slot::Slot;
use super::Delivery;
use crate::platform::command;
use crate::proto::{Sink, Told};

/// One `zellij pipe` that the daemon holds open, and the slot that feeds it.
///
/// The writer shares the child, so that it can ask whether the process still
/// runs. The reader needs no such handle. The end of the stdout stream tells it
/// the same thing.
pub struct Held {
    child: Arc<Mutex<Child>>,
    slot: Arc<Slot>,
}

impl Held {
    /// Whether the pipe process still runs. This question does not wait.
    pub fn alive(&self) -> bool {
        matches!(
            self.child
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .try_wait(),
            Ok(None)
        )
    }

    /// Queues one payload, which replaces whatever had not gone yet.
    pub fn fill(&self, line: String) {
        self.slot.fill(line);
    }

    /// Asks for an empty message, so that the plugins on this pipe get a turn.
    pub fn nudge(&self) {
        self.slot.nudge();
    }
}

/// Opens one pipe into one session, with a thread to write to it and a thread
/// to read from it.
///
/// Side effect: this function spawns a process and two threads. The command
/// carries no payload argument, which is what makes it read its stdin and stay
/// open.
pub fn open(
    session: &str,
    reported: &Sender<(Sink, Delivery)>,
    told: &Sender<Told>,
) -> Option<Held> {
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

    let writing = (Arc::clone(&slot), Arc::clone(&child), reported.clone());
    thread::spawn(move || {
        let (slot, child, reported) = writing;
        write_until_closed(&sink, stdin, &slot, &child, &reported);
    });
    let told = told.clone();
    thread::spawn(move || super::read_until_ended(stdout, &told));

    Some(Held { child, slot })
}

/// Kills one held pipe, waits for it, and stops its writer.
///
/// The wait matters as much as the kill. A process that receives a signal and is
/// not reaped stays in the table as a zombie. A daemon runs for as long as the
/// user stays logged in.
pub fn shut(held: &Held) {
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
    use std::io::Write;
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
