//! What one writer waits for, and the payload that it writes next.
//!
//! Every transport that the daemon holds open writes from a thread of its own,
//! and every one of them waits here. A caller fills the slot and returns, so a
//! client that reads slowly delays that client and no other.

use std::sync::{Condvar, Mutex, MutexGuard};

/// What a writer waits for, and the payload that it writes next.
///
/// The slot holds one payload rather than a queue. Every delivery carries the
/// whole state, so a payload that a newer one replaced is a payload that nobody
/// needs. A client that reads slowly therefore costs memory that does not grow,
/// and it receives the newest state rather than the oldest.
#[derive(Default)]
pub struct Slot {
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
    /// The writer is finished with. A writer that finds this returns.
    Closed,
}

impl Slot {
    /// Puts one payload in, and replaces whatever waits there.
    pub fn fill(&self, payload: String) {
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
    pub fn nudge(&self) {
        let mut waiting = self.held();
        if *waiting != Waiting::Nothing {
            return;
        }
        *waiting = Waiting::Nudge;
        self.told.notify_one();
    }

    /// Says that there is nothing more to write, and wakes the writer so that
    /// it stops.
    pub fn close(&self) {
        *self.held() = Waiting::Closed;
        self.told.notify_one();
    }

    /// Waits for a payload and takes it. `None` says that the slot is closed.
    pub fn take(&self) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::sync::Arc;
    use std::thread;

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
