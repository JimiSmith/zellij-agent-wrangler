//! This module tells every watcher the last thing that the daemon did.
//!
//! A watcher is a connection that asks for every record. When there is no
//! watcher, this module costs nothing. A closure builds the record, and nothing
//! calls that closure until somebody listens.
//!
//! Nothing that the daemon does waits on a watcher. A watcher exists to
//! diagnose a daemon whose problem is one thing that waits on another. A watcher
//! that cannot keep up therefore loses records, and it learns how many records
//! it lost.

use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Mutex, MutexGuard};

use crate::proto::{Watched, What};

/// The number of records that a watcher falls behind by before it loses records.
///
/// A watcher that writes to a file or to a pipe keeps up with everything that
/// the daemon produces. This headroom is for a watcher that a person reads. It
/// is far more than a burst fills before the reader gets to it.
const BACKLOG: usize = 4096;

/// One watcher: where to hand records to, and how many records it did not get.
struct Watcher {
    to: SyncSender<Watched>,
    /// The records dropped since the last record that got through. Once there
    /// is room again, the watcher gets a record of its own for them.
    missed: u64,
}

/// Everybody who watches what the daemon does.
#[derive(Default)]
pub struct Watchers {
    who: Mutex<Vec<Watcher>>,
}

impl Watchers {
    /// Takes the state, whatever happened to the last holder of it.
    ///
    /// A watcher is a diagnostic. A daemon that answers nothing is too high a
    /// price for a diagnostic, so this method carries on with a poisoned lock.
    fn held(&self) -> MutexGuard<'_, Vec<Watcher>> {
        self.who.lock().unwrap_or_else(|held| held.into_inner())
    }

    /// Takes a new watcher, and gives back the end that it reads from.
    ///
    /// When a watcher drops the receiver, it leaves. The next record finds the
    /// watcher gone, and this module forgets it then. The watcher says nothing.
    pub fn watch(&self) -> Receiver<Watched> {
        let (to, from) = sync_channel(BACKLOG);
        self.held().push(Watcher { to, missed: 0 });
        from
    }

    /// Says what happened.
    ///
    /// This method returns at once, whoever watches and however slowly they
    /// read. When nobody watches, nothing builds the record at all.
    pub fn saw(&self, what: impl FnOnce() -> What) {
        let mut who = self.held();
        if who.is_empty() {
            return;
        }
        let record = Watched {
            at: crate::now(),
            what: what(),
        };
        who.retain_mut(|watcher| watcher.tell(&record));
    }
}

impl Watcher {
    /// Hands one record over. If this watcher went away, the answer is `false`.
    ///
    /// The watcher gets what it missed before it gets the new record. The run
    /// that it reads then shows where the hole is, and not only that a hole
    /// exists.
    fn tell(&mut self, record: &Watched) -> bool {
        if self.missed > 0 {
            let missed = Watched {
                at: record.at,
                what: What::Missed {
                    records: self.missed,
                },
            };
            match self.to.try_send(missed) {
                Ok(()) => self.missed = 0,
                Err(TrySendError::Full(_)) => {
                    self.missed += 1;
                    return true;
                }
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
        match self.to.try_send(record.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.missed += 1;
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked() -> What {
        What::Asked
    }

    #[test]
    fn nothing_is_built_when_nobody_is_watching() {
        let watchers = Watchers::default();
        watchers.saw(|| panic!("a record was built for nobody"));
    }

    #[test]
    fn a_watcher_is_told_what_the_daemon_did() {
        let watchers = Watchers::default();
        let from = watchers.watch();
        watchers.saw(asked);
        assert_eq!(from.recv().unwrap().what, What::Asked);
    }

    #[test]
    fn a_watcher_that_has_gone_is_forgotten() {
        let watchers = Watchers::default();
        drop(watchers.watch());
        watchers.saw(asked);
        assert!(watchers.held().is_empty());
    }

    #[test]
    fn a_watcher_that_falls_behind_is_told_what_it_missed() {
        let watchers = Watchers::default();
        let from = watchers.watch();
        // One record more than the watcher holds, so it loses exactly one.
        for _ in 0..BACKLOG + 1 {
            watchers.saw(asked);
        }
        for _ in 0..BACKLOG {
            assert_eq!(from.recv().unwrap().what, What::Asked);
        }
        // The daemon owes the hole, and hands it over before the next record.
        watchers.saw(asked);
        assert_eq!(from.recv().unwrap().what, What::Missed { records: 1 });
        assert_eq!(from.recv().unwrap().what, What::Asked);
    }

    #[test]
    fn a_daemon_that_lost_a_watcher_mid_record_carries_on() {
        // The whole point of a harmless watcher. A watcher is a diagnostic, and
        // a daemon that one watcher stops is worse than no diagnostic at all.
        let watchers = Watchers::default();
        let staying = watchers.watch();
        drop(watchers.watch());
        watchers.saw(asked);
        assert_eq!(staying.recv().unwrap().what, What::Asked);
        assert_eq!(watchers.held().len(), 1);
    }
}
