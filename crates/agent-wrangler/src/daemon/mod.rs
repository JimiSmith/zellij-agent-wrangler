//! The daemon: one per user, started by whichever hook first finds none
//! running.
//!
//! One connection is one thread, and the state behind one lock, because the
//! traffic is a handful of messages a second and a lock is far less machinery
//! than a core loop with a channel. What the lock does *not* cover is the point:
//! reading an agent's files, running a client, and writing the state out all
//! happen with it released, because each of them can take arbitrarily long and
//! the daemon answers its own socket while it is stuck, so nothing else could
//! take over from a daemon that froze holding it.
//!
//! Delivering is the one thing not done by whichever thread caused it. Every
//! client is sent the whole state rather than what changed, so a thread that
//! applied something says the clients are [`Owed`] one and carries on, and a
//! single thread does the delivering. Any number of changes arriving during one
//! delivery is one delivery after it, which is what keeps a working agent's
//! burst of events from being a burst of programs run.

pub mod notify;
pub mod persist;
pub mod sink;
pub mod state;

use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, Listener, ListenerOptions, Stream};

use agent_wrangler_core::agent::FORMAT;
use agent_wrangler_core::notify::Notifier;

use crate::daemon::state::{look, read_hook, Call, Client, Real, State};
use crate::paths;
use crate::proto::{read_message, write_message, Inbound, Outbound, Sink};

/// How often every held transcript is looked at again, and every held pid.
///
/// A second is what the eye reads as immediate for something that is not a
/// keystroke, and one stat per session at that rate costs nothing measurable.
const POLL: Duration = Duration::from_secs(1);

/// How long to wait before accepting again after a failure.
///
/// A failure such as running out of descriptors does not consume the connection
/// that caused it, so the same error is waiting on the next call and an
/// immediate retry is a spin. This is short enough not to matter when the
/// failure was a one-off.
const AFTER_REFUSAL: Duration = Duration::from_millis(100);

/// Take the state, whatever happened to whoever held it last.
///
/// A thread that panicked holding this poisons it, and every later attempt would
/// fail forever after. Since the daemon keeps answering its socket either way,
/// nothing could replace it, so a poisoned lock would leave a daemon that is
/// alive, reachable and permanently useless. What it guards is a set of records
/// re-sent whole on every change, so carrying on with it recovers where refusing
/// to does not.
fn held(shared: &Arc<Mutex<State>>) -> MutexGuard<'_, State> {
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether the clients are owed the state, and whoever is waiting to hand it to
/// them.
///
/// A delivery runs a program per client and waits for each, so it takes as long
/// as the slowest of them, and every event that happens meanwhile would
/// otherwise start a delivery of its own. What the clients are sent is the whole
/// state rather than what changed, so a hundred changes during one delivery are
/// one delivery after it and lose nothing: the next one carries all of them.
///
/// This is what keeps the number of programs run a function of how fast they
/// can be run, rather than of how fast the agents are reporting. A burst of
/// events is what a working agent looks like.
#[derive(Default)]
struct Owed {
    owed: Mutex<bool>,
    told: Condvar,
}

impl Owed {
    /// Say the clients are owed the state. Returns at once, whoever is
    /// delivering and however long they take.
    fn owe(&self) {
        let mut owed = self.owed.lock().unwrap_or_else(|e| e.into_inner());
        *owed = true;
        self.told.notify_one();
    }

    /// Wait until something is owed, and take it. Every change made up to this
    /// moment is covered by the delivery that follows.
    fn take(&self) {
        let mut owed = self.owed.lock().unwrap_or_else(|e| e.into_inner());
        while !*owed {
            owed = self
                .told
                .wait(owed)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *owed = false;
    }
}

/// What the daemon does with the socket name.
enum Bound {
    /// This process is the daemon.
    Ours(Listener),
    /// Another process already is, so there is nothing to do.
    Taken,
}

/// Claim the socket name, or find that someone else holds it.
///
/// Connecting first is what tells a live daemon from a name left behind by one
/// that died. Taking the name over is then safe *because* of that: nothing
/// answered it, so nothing is listening on it. Without this a daemon killed
/// outright leaves a name that can never be bound again, on every system where
/// the name is a file rather than one the kernel drops with the process.
fn bind() -> std::io::Result<Bound> {
    let name = paths::socket_name();
    let ns = name.to_ns_name::<GenericNamespaced>()?;
    if Stream::connect(ns.clone()).is_ok() {
        return Ok(Bound::Taken);
    }
    ListenerOptions::new()
        .name(ns)
        .try_overwrite(true)
        .create_sync()
        .map(Bound::Ours)
}

/// Write the state out and tell every client, dropping the ones that have gone.
///
/// Side effect: writes a file and runs a program per zellij client. Called with
/// no lock held, and takes it only to read what to send and to note who could
/// not be reached.
fn publish(shared: &Arc<Mutex<State>>, dir: &Path) {
    let (payload, clients, saved) = {
        let state = held(shared);
        (state.payload(), state.clients(), state.snapshot())
    };
    persist::save(dir, &saved, &clients);
    let answers: Vec<(Sink, bool)> = clients
        .into_iter()
        .map(|client| {
            let sent = sink::deliver(&client.sink, &payload).sent();
            (client.sink, sent)
        })
        .collect();
    let mut state = held(shared);
    for (sink, sent) in answers {
        match sent {
            true => state.reached(&sink),
            false => {
                state.missed(&sink);
            }
        }
    }
}

/// Tell one client, whatever the rest of them are doing.
fn deliver_one(shared: &Arc<Mutex<State>>, to: &Sink) {
    let payload = held(shared).payload();
    let sent = sink::deliver(to, &payload).sent();
    let mut state = held(shared);
    match sent {
        true => state.reached(to),
        false => {
            state.missed(to);
        }
    }
}

/// Write out the state and who is listening to it, without telling anyone.
fn record(shared: &Arc<Mutex<State>>, dir: &Path) {
    let (saved, clients) = {
        let state = held(shared);
        (state.snapshot(), state.clients())
    };
    persist::save(dir, &saved, &clients);
}

/// Say a call out loud, wherever the user is.
///
/// Side effect: runs a program per notifier registered, and waits for each. It
/// is done here rather than by the clients because every client is handed the
/// same call: one that raised its own would raise it once per client, and the
/// count would go up with every sidebar the user opened.
///
/// Called with no lock held, and takes it only to read what to run.
fn announce(shared: &Arc<Mutex<State>>, call: &Call) {
    for notifier in held(shared).notifiers() {
        notify::raise(&notifier, call);
    }
}

/// Read one connection to its end, applying what it says.
///
/// Each message is owed as it is applied rather than at the end of the
/// connection, so a client that holds its socket open is not also holding back
/// every change it has already reported.
///
/// Returns `true` when the sender spoke a record format this build does not,
/// which is the one condition that makes the daemon stand down.
fn serve(stream: Stream, shared: &Arc<Mutex<State>>, owed: &Owed, dir: &Path) -> bool {
    let mut reader = BufReader::new(&stream);

    while let Ok(Some(message)) = read_message::<_, Inbound>(&mut reader) {
        match message {
            Inbound::Hook { format, .. } | Inbound::Register { format, .. } if format != FORMAT => {
                return true;
            }
            Inbound::Hook { hook, .. } => {
                // Read before locking. A transcript on a mount that has stopped
                // answering takes as long as it takes, and every other event on
                // the machine carries on meanwhile.
                let reading = read_hook(&hook, &Real);
                let applied = held(shared).apply_hook(&hook, reading);
                if applied.changed() {
                    owed.owe();
                }
                // Owed first and announced after, which is the order that
                // matters: a notifier that hangs cannot hold up the state
                // saying the same thing, because saying it is owed is all this
                // thread does about it. Which of the two the user meets first
                // is whichever of a delivery and a notifier finishes first, and
                // neither waits on the other.
                if let Some(call) = applied.call() {
                    announce(shared, call);
                }
            }
            Inbound::Register { sink, notify, .. } => {
                held(shared).register(Client {
                    sink: sink.clone(),
                    notify: Notifier::new(notify),
                });
                // A client that has just registered has nothing yet, so it is
                // told the state whether or not anything changed.
                deliver_one(shared, &sink);
                record(shared, dir);
            }
            Inbound::Seen { session } => {
                if held(shared).on_seen(&session) {
                    owed.owe();
                }
            }
            Inbound::Snapshot => {
                let payload = held(shared).payload();
                let mut writer = BufWriter::new(&stream);
                let _ = write_message(
                    &mut writer,
                    &Outbound::Agents {
                        format: FORMAT,
                        records: payload,
                    },
                );
            }
        }
    }
    false
}

/// Look at every held transcript and pid, and take in what changed.
fn sweep(shared: &Arc<Mutex<State>>, owed: &Owed) {
    let (plan, since) = {
        let state = held(shared);
        (state.plan(), state.mtimes())
    };
    // The looking is the slow part, and it holds nothing.
    let found = look(&plan, &Real, &since);
    if held(shared).observe(found) {
        owed.owe();
    }
}

/// Run as the daemon until something says to stop.
///
/// Returns as soon as the name is already claimed, so a hook that starts one
/// unnecessarily costs a process that exits rather than a second daemon.
pub fn run() -> std::io::Result<()> {
    let listener = match bind()? {
        Bound::Taken => return Ok(()),
        Bound::Ours(listener) => listener,
    };

    let dir = paths::state_dir();
    let mut initial = State::default();
    let (sessions, clients) = persist::load(&dir);
    initial.restore(sessions, &Real);
    for client in clients {
        initial.register(client);
    }
    let shared = Arc::new(Mutex::new(initial));
    let stop = Arc::new(AtomicBool::new(false));
    let owed = Arc::new(Owed::default());

    {
        let shared = Arc::clone(&shared);
        let owed = Arc::clone(&owed);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(POLL);
                sweep(&shared, &owed);
            }
        });
    }

    // One thread does the delivering, so however many events arrive while a
    // delivery is running, they are one delivery afterwards rather than one
    // each.
    {
        let shared = Arc::clone(&shared);
        let owed = Arc::clone(&owed);
        let dir = dir.clone();
        thread::spawn(move || loop {
            owed.take();
            publish(&shared, &dir);
        });
    }

    for incoming in listener.incoming() {
        let Ok(stream) = incoming else {
            // Whatever refused this is usually still there on the next call, so
            // retrying at once is a spin rather than a retry.
            thread::sleep(AFTER_REFUSAL);
            continue;
        };
        let shared = Arc::clone(&shared);
        let owed = Arc::clone(&owed);
        let stop = Arc::clone(&stop);
        let dir = dir.clone();
        // A connection gets a thread, so one client asking for a snapshot, or
        // one delivery that is slow to run, cannot hold up the next hook.
        thread::spawn(move || {
            if serve(stream, &shared, &owed, &dir) {
                // The other end is a different build of this program, so what it
                // says cannot be read reliably and what this says cannot be read
                // by it. Standing down leaves the name free for the daemon it
                // expects, which is itself; the next event of any kind starts
                // that one. What has been applied is written out first, and the
                // clients with it, so the daemon taking over inherits both.
                stop.store(true, Ordering::Relaxed);
                record(&shared, &dir);
                std::process::exit(0);
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_poll_is_often_enough_to_read_as_immediate() {
        assert!(POLL <= Duration::from_secs(1));
    }

    #[test]
    fn a_refused_connection_is_not_retried_at_once() {
        // The whole point of the pause: whatever refused it is still there, so
        // an immediate retry saturates a core rather than recovering.
        assert!(AFTER_REFUSAL >= Duration::from_millis(50));
    }

    #[test]
    fn every_change_made_before_a_delivery_is_covered_by_it() {
        // The whole point of owing rather than delivering: what the clients are
        // sent is the state entire, so three changes during one delivery are
        // one delivery afterwards rather than three.
        let owed = Arc::new(Owed::default());
        for _ in 0..3 {
            owed.owe();
        }
        owed.take();

        let waiting = Arc::clone(&owed);
        let (delivered, heard) = std::sync::mpsc::channel();
        thread::spawn(move || {
            waiting.take();
            let _ = delivered.send(());
        });
        assert!(
            heard.recv_timeout(Duration::from_millis(100)).is_err(),
            "the three were taken together, so nothing is owed"
        );

        owed.owe();
        assert!(
            heard.recv_timeout(Duration::from_secs(5)).is_ok(),
            "a change after the delivery is owed like any other"
        );
    }

    #[test]
    fn a_poisoned_lock_still_gives_up_its_state() {
        let shared = Arc::new(Mutex::new(State::default()));
        let poisoner = Arc::clone(&shared);
        let _ = thread::spawn(move || {
            let _guard = poisoner.lock().expect("state lock");
            panic!("a thread died holding the state");
        })
        .join();
        assert!(shared.is_poisoned());
        // A daemon that refused to carry on here would be alive, reachable, and
        // permanently unable to answer anything.
        assert!(held(&shared).clients().is_empty());
    }
}
