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
pub mod watch;

use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, Listener, ListenerOptions, Stream};

use agent_wrangler_core::agent::FORMAT;
use agent_wrangler_core::notify::Notifier;

use crate::daemon::notify::Announced;
use crate::daemon::sink::Delivery;
use crate::daemon::state::{look, read_hook, Call, Client, Real, State};
use crate::daemon::watch::Watchers;
use crate::paths;
use crate::proto::{read_message, write_message, Inbound, Outbound, Sink, What};

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
fn publish(shared: &Arc<Mutex<State>>, dir: &Path, watchers: &Watchers) {
    let (payload, clients, saved) = {
        let state = held(shared);
        (state.payload(), state.clients(), state.snapshot())
    };
    let agents = saved.len();
    persist::save(dir, &saved, &clients);
    let answers: Vec<(Sink, Delivery)> = clients
        .into_iter()
        .map(|client| {
            watchers.saw(|| What::Delivering {
                sink: client.sink.clone(),
                agents,
            });
            let began = Instant::now();
            let delivery = sink::deliver(&client.sink, &payload);
            let took = began.elapsed().as_millis() as u64;
            let (sent, abandoned) = match delivery {
                Delivery::Sent => (true, false),
                Delivery::Failed => (false, false),
                Delivery::Abandoned => (true, true),
            };
            watchers.saw(|| What::Delivered {
                sink: client.sink.clone(),
                sent,
                abandoned,
                took,
            });
            (client.sink, delivery)
        })
        .collect();
    let mut state = held(shared);
    for (sink, delivery) in answers {
        answered(&mut state, &sink, delivery);
    }
}

/// What one delivery says about the client it was for.
///
/// Only a refusal counts against a client, and it is the answer a client that
/// has gone gives: piping into a session that is not there comes back in
/// milliseconds saying so. A delivery given up on says nothing about whether
/// the client is there, and a great deal about what the multiplexer is doing,
/// so counting it would retire a live sidebar for the multiplexer's fault - and
/// a sidebar, once dropped, never asks again.
fn answered(state: &mut State, sink: &Sink, delivery: Delivery) {
    match delivery {
        Delivery::Sent | Delivery::Abandoned => state.reached(sink),
        Delivery::Failed => {
            state.missed(sink);
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

/// Say a call out loud, wherever the user is, if this is a moment to say
/// anything at all.
///
/// Side effect: runs a program per notifier registered, and waits for each. It
/// is done here rather than by the clients because every client is handed the
/// same call: one that raised its own would raise it once per client, and the
/// count would go up with every sidebar the user opened.
///
/// Whether a call is one to say out loud is `announced`'s to answer, and a call
/// it turns down costs nothing else: every client is owed the state before this
/// is reached, so what is drawn is the same either way.
///
/// Called with no lock held, and takes it only to read what to run. What to run
/// is bound before the loop rather than iterated straight from the guard,
/// because a notifier is waited for and the state is not this thread's to hold
/// while that happens.
fn announce(shared: &Arc<Mutex<State>>, call: &Call, announced: &Mutex<Announced>) {
    let speaking = announced
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .worth_saying(Instant::now());
    if !speaking {
        return;
    }
    let notifiers = held(shared).notifiers();
    for notifier in notifiers {
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
fn serve(
    stream: Stream,
    shared: &Arc<Mutex<State>>,
    owed: &Owed,
    dir: &Path,
    watchers: &Watchers,
    announced: &Mutex<Announced>,
) -> bool {
    let mut reader = BufReader::new(&stream);

    while let Ok(Some(message)) = read_message::<_, Inbound>(&mut reader) {
        match message {
            Inbound::Hook { format, .. }
            | Inbound::Register { format, .. }
            | Inbound::Monitor { format }
                if format != FORMAT =>
            {
                return true;
            }
            Inbound::Hook { hook, .. } => {
                // Read before locking. A transcript on a mount that has stopped
                // answering takes as long as it takes, and every other event on
                // the machine carries on meanwhile.
                let reading = read_hook(&hook, &Real);
                let applied = held(shared).apply_hook(&hook, reading);
                watchers.saw(|| What::Hook {
                    agent: hook.agent.clone(),
                    event: hook.event.clone(),
                    session: hook.session_id.clone(),
                    told: applied.changed(),
                });
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
                    announce(shared, call, announced);
                }
            }
            Inbound::Register { sink, notify, .. } => {
                watchers.saw(|| What::Registered { sink: sink.clone() });
                held(shared).register(Client {
                    sink,
                    notify: Notifier::new(notify),
                });
                // A client that has just registered has nothing yet, so it is
                // owed the state whether or not anything changed.
                //
                // Owed rather than delivered here, even though only one client
                // asked. A layout with a sidebar in every tab registers every
                // one of them the moment the user attaches, each on a
                // connection thread of its own, and a delivery apiece is that
                // many multiplexer clients started at once - which is the load
                // under which one of them wedges. One delivery answers all of
                // them, because what a client is sent is the whole state.
                owed.owe();
                record(shared, dir);
            }
            Inbound::Seen { session } => {
                let told = held(shared).on_seen(&session);
                watchers.saw(|| What::Seen {
                    session: session.clone(),
                    told,
                });
                if told {
                    owed.owe();
                }
            }
            Inbound::Snapshot => {
                watchers.saw(|| What::Asked);
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
            // The connection becomes the watcher's, and is read no further: what
            // it asked for is everything from now on, and it says nothing else.
            // It leaves by going away, which the first record after that finds.
            Inbound::Monitor { .. } => {
                let from = watchers.watch();
                let mut writer = BufWriter::new(&stream);
                while let Ok(record) = from.recv() {
                    if write_message(&mut writer, &record).is_err() {
                        break;
                    }
                }
                return false;
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
    let watchers = Arc::new(Watchers::default());
    // One per daemon rather than one per connection: a hook gets a thread of
    // its own, so a quiet kept per connection would be no quiet at all.
    let announced = Arc::new(Mutex::new(Announced::default()));

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
        let watchers = Arc::clone(&watchers);
        let dir = dir.clone();
        thread::spawn(move || loop {
            owed.take();
            publish(&shared, &dir, &watchers);
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
        let watchers = Arc::clone(&watchers);
        let announced = Arc::clone(&announced);
        let dir = dir.clone();
        // A connection gets a thread, so one client asking for a snapshot, or
        // one delivery that is slow to run, cannot hold up the next hook.
        thread::spawn(move || {
            if serve(stream, &shared, &owed, &dir, &watchers, &announced) {
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
    use crate::daemon::state::REFUSALS;

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

    fn sidebar() -> Client {
        Client {
            sink: Sink::Zellij {
                session: "proto".to_string(),
            },
            notify: None,
        }
    }

    #[test]
    fn a_delivery_given_up_on_never_retires_the_client_it_was_for() {
        // The failure this rule exists for: a sidebar is alive, is being drawn
        // to, and the multiplexer will not let go of the program that draws to
        // it. Counting those would drop the client after three of them, and
        // nothing would ever register it again, so the sidebar would be left
        // showing whatever it last received for as long as the session lasted.
        let mut state = State::default();
        let client = sidebar();
        state.register(client.clone());
        for _ in 0..100 {
            answered(&mut state, &client.sink, Delivery::Abandoned);
        }
        assert_eq!(state.clients(), vec![client]);
    }

    #[test]
    fn a_client_that_keeps_refusing_is_still_given_up_on() {
        // The other half of it: a session that has gone answers at once and
        // says so, and that is the answer a client is retired for.
        let mut state = State::default();
        let client = sidebar();
        state.register(client.clone());
        for _ in 0..REFUSALS {
            answered(&mut state, &client.sink, Delivery::Failed);
        }
        assert!(state.clients().is_empty());
    }

    #[test]
    fn a_delivery_given_up_on_forgives_the_refusals_before_it() {
        // It is a client that took the state, so the count towards retiring it
        // starts again like any delivery that landed. Each run is one short of
        // retiring the client, so the two of them are two apart only if the
        // one in between forgave what came before it.
        let mut state = State::default();
        let client = sidebar();
        state.register(client.clone());
        for _ in 1..REFUSALS {
            answered(&mut state, &client.sink, Delivery::Failed);
        }
        answered(&mut state, &client.sink, Delivery::Abandoned);
        for _ in 1..REFUSALS {
            answered(&mut state, &client.sink, Delivery::Failed);
        }
        assert_eq!(state.clients(), vec![client]);
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
