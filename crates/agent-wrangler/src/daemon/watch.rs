//! Telling whoever is watching what the daemon just did.
//!
//! A watcher is a connection asking to be told everything, and nothing here
//! costs anything when there is none: what a record would say is built by a
//! closure that is not called until somebody is listening.
//!
//! Nothing the daemon does waits on a watcher. The whole reason for watching is
//! a daemon whose problem is things waiting on each other, so a watcher that
//! cannot keep up loses records and is told how many, rather than being waited
//! for.

use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Mutex, MutexGuard};

use crate::proto::{Watched, What};

/// How many records a watcher may fall behind by before it starts losing them.
///
/// A watcher writing to a file or a pipe keeps up with anything the daemon can
/// produce; this is headroom for one that is being read by a person, and is far
/// more than a burst could fill before the reader gets to it.
const BACKLOG: usize = 4096;

/// One watcher: where to hand records, and how many it did not get.
struct Watcher {
    to: SyncSender<Watched>,
    /// Records dropped since the last one that got through, which are told to
    /// it as a record of their own once there is room again.
    missed: u64,
}

/// Everyone watching what the daemon does.
#[derive(Default)]
pub struct Watchers {
    who: Mutex<Vec<Watcher>>,
}

impl Watchers {
    /// Take the state, whatever happened to whoever held it last.
    ///
    /// A watcher is a diagnostic. It is not worth a daemon that has stopped
    /// answering, so a lock poisoned while it was held is carried on with.
    fn held(&self) -> MutexGuard<'_, Vec<Watcher>> {
        self.who.lock().unwrap_or_else(|held| held.into_inner())
    }

    /// Take a new watcher, and give back the end it reads from.
    ///
    /// Dropping the receiver is how a watcher leaves: the next record finds it
    /// gone and it is forgotten then, rather than needing to say so.
    pub fn watch(&self) -> Receiver<Watched> {
        let (to, from) = sync_channel(BACKLOG);
        self.held().push(Watcher { to, missed: 0 });
        from
    }

    /// Say what just happened.
    ///
    /// Returns at once, whoever is watching and however slowly they read. The
    /// record is not built at all when nobody is.
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
    /// Hand one record over. `false` when this watcher has gone.
    ///
    /// What it missed is owed to it before what just happened, so the run it
    /// reads says where the hole is rather than only that there was one.
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
        // One more than it can hold, so exactly one is lost.
        for _ in 0..BACKLOG + 1 {
            watchers.saw(asked);
        }
        for _ in 0..BACKLOG {
            assert_eq!(from.recv().unwrap().what, What::Asked);
        }
        // The hole is owed, and is handed over before whatever comes next.
        watchers.saw(asked);
        assert_eq!(from.recv().unwrap().what, What::Missed { records: 1 });
        assert_eq!(from.recv().unwrap().what, What::Asked);
    }

    #[test]
    fn a_daemon_that_lost_a_watcher_mid_record_carries_on() {
        // The whole point of watching being harmless: it is a diagnostic, and a
        // daemon that could be stopped by one would be worse than no diagnostic.
        let watchers = Watchers::default();
        let staying = watchers.watch();
        drop(watchers.watch());
        watchers.saw(asked);
        assert_eq!(staying.recv().unwrap().what, What::Asked);
        assert_eq!(watchers.held().len(), 1);
    }
}
