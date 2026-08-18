//! The daemon: one per user. The first hook that finds no daemon starts one.
//!
//! One connection is one thread, and the state sits behind one lock. The
//! traffic is a handful of messages a second, and a lock is far less machinery
//! than a core loop with a channel. What the lock does *not* cover is the
//! point. The daemon reads an agent's files, runs a client, and writes the
//! state out with the lock released. Each of these can take arbitrarily long.
//! The daemon answers its own socket while it is stuck, so nothing else can
//! take over from a daemon that froze under the lock.
//!
//! The delivery is the one thing that the thread behind a change does not do.
//! Every client is sent the whole state rather than what changed. A thread that
//! applied something says that the clients are [`Owed`] one, and then carries
//! on. One thread makes every delivery. Any number of changes during one
//! delivery is one delivery after it. The number of programs run is therefore
//! a function of the delivery, and not of the burst of events from a busy
//! agent.

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

/// How often the daemon looks at every held transcript again, and at every
/// held pid.
///
/// A second is what the eye reads as immediate for something that is not a
/// keystroke. One stat per session at that rate costs nothing measurable.
const POLL: Duration = Duration::from_secs(1);

/// How long to wait after a failure, before the daemon accepts again.
///
/// A failure such as an exhausted set of descriptors does not consume the
/// connection that caused it. The same error waits on the next call, and an
/// immediate retry is a spin. If the failure was a one-off, this delay is short
/// enough not to matter.
const AFTER_REFUSAL: Duration = Duration::from_millis(100);

/// This function takes the state, whatever happened to whoever held it last.
///
/// A thread that panics under this lock poisons it, and every later attempt
/// fails from then on. The daemon answers its socket either way, so nothing can
/// replace it. A poisoned lock therefore leaves a daemon that is alive,
/// reachable and permanently useless. The lock guards a set of records that are
/// re-sent whole on every change. A daemon that carries on recovers. A daemon
/// that refuses does not.
fn held(shared: &Arc<Mutex<State>>) -> MutexGuard<'_, State> {
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether the clients are owed the state, and who waits to hand it to them.
///
/// A delivery runs a program for each client and waits for each one, so it
/// takes as long as the slowest of them. Without this flag, every event in that
/// time starts a delivery of its own. The clients are sent the whole state
/// rather than what changed. A hundred changes during one delivery are
/// therefore one delivery after it, and they lose nothing. The next delivery
/// carries all of them.
///
/// The number of programs run is a function of how fast they can run, and not
/// of how fast the agents report. A burst of events is what a busy agent looks
/// like.
#[derive(Default)]
struct Owed {
    owed: Mutex<bool>,
    told: Condvar,
}

impl Owed {
    /// This method says that the clients are owed the state. It returns at
    /// once, whoever delivers and however long they take.
    fn owe(&self) {
        let mut owed = self.owed.lock().unwrap_or_else(|e| e.into_inner());
        *owed = true;
        self.told.notify_one();
    }

    /// This method waits until something is owed, and takes it. The delivery
    /// that follows covers every change made up to that moment.
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

/// This function claims the socket name, or finds that someone else holds it.
///
/// A connection attempt first tells a live daemon from a name that a dead one
/// left behind. To take the name over is then safe *because* of that. Nothing
/// answered the name, so nothing listens on it. Without this attempt, a daemon
/// that was killed outright leaves a name that nothing can bind again. This is
/// true on every system where the name is a file rather than a name that the
/// kernel drops with the process.
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

/// This function writes the state out and tells every client. It drops the
/// clients that went away.
///
/// Side effect: it writes a file and runs a program for each zellij client. The
/// caller holds no lock. This function takes the lock only to read what to
/// send, and to note the clients that it did not reach.
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
/// Only a refusal counts against a client. A refusal is the answer that a
/// client that went away gives. A pipe into a session that is not there comes
/// back in milliseconds and says so. A delivery that was given up on says
/// nothing about whether the client is there. It says a great deal about the
/// state of the multiplexer. A count of those deliveries retires a live sidebar
/// for the fault of the multiplexer, and a sidebar that was dropped never asks
/// again.
fn answered(state: &mut State, sink: &Sink, delivery: Delivery) {
    match delivery {
        Delivery::Sent | Delivery::Abandoned => state.reached(sink),
        Delivery::Failed => {
            state.missed(sink);
        }
    }
}

/// This function writes out the state and who listens to it. It tells nobody.
fn record(shared: &Arc<Mutex<State>>, dir: &Path) {
    let (saved, clients) = {
        let state = held(shared);
        (state.snapshot(), state.clients())
    };
    persist::save(dir, &saved, &clients);
}

/// This function says a call out loud, wherever the user is. If this is not a
/// moment to say anything, it says nothing.
///
/// Side effect: it runs a program for each notifier registered, and waits for
/// each one. The daemon does this here rather than in the clients, because
/// every client holds the same call. A client that raises its own notification
/// raises it once for each client, and the count goes up with every sidebar
/// that the user opens.
///
/// `announced` answers whether a call is one to say out loud. A call that it
/// turns down costs nothing else. Every client is owed the state before this
/// point, so what is drawn is the same either way.
///
/// The caller holds no lock. This function takes the lock only to read what to
/// run. What to run is bound before the loop rather than read straight from the
/// guard, because the loop waits for a notifier. The state is not this thread's
/// to hold during that wait.
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

/// This function reads one connection to its end, and applies what it says.
///
/// Each message is owed as the daemon applies it, rather than at the end of the
/// connection. A client that holds its socket open therefore does not hold back
/// every change that it already reported.
///
/// If the sender spoke a record format that this build does not, this function
/// returns `true`. That is the one condition that makes the daemon stand down.
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
                // The read happens before the lock. A transcript on a mount
                // that no longer answers takes as long as it takes, and every
                // other event on the machine carries on meanwhile.
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
                // The order matters: the state is owed first, and the call is
                // announced after. A notifier that hangs cannot hold up the
                // state, because this thread only says that the state is owed.
                // The user meets whichever of the delivery and the notifier
                // finishes first, and neither one waits on the other.
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
                // A client that just registered has nothing yet, so it is owed
                // the state whether or not anything changed.
                //
                // The state is owed here rather than delivered, even though
                // only one client asked. A layout with a sidebar in every tab
                // registers every one of them the moment that the user
                // attaches. Each sidebar registers on a connection thread of
                // its own. A delivery for each one starts that many multiplexer
                // clients at once, which is the load that wedges one of them.
                // One delivery answers all of them, because a client is sent
                // the whole state.
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
            // The connection becomes the watcher's, and the daemon reads it no
            // further. What it asked for is everything from now on, and it says
            // nothing else. A watcher that goes away is gone, and the first
            // record after that finds it.
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

/// This function looks at every held transcript and pid, and takes in what
/// changed.
fn sweep(shared: &Arc<Mutex<State>>, owed: &Owed) {
    let (plan, since) = {
        let state = held(shared);
        (state.plan(), state.mtimes())
    };
    // The look is the slow part, and it holds nothing.
    let found = look(&plan, &Real, &since);
    if held(shared).observe(found) {
        owed.owe();
    }
}

/// This function runs as the daemon until something says to stop.
///
/// If another process already claimed the name, this function returns at once.
/// A hook that starts a daemon without need therefore costs one process that
/// exits, rather than a second daemon.
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
    // One per daemon rather than one per connection. A hook gets a thread of
    // its own. A quiet period per connection is therefore no quiet period at
    // all.
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

    // One thread makes every delivery. However many events arrive during one
    // delivery, they are one delivery afterwards rather than one delivery each.
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
            // Whatever refused this is usually still there on the next call.
            // An immediate retry is therefore a spin rather than a retry.
            thread::sleep(AFTER_REFUSAL);
            continue;
        };
        let shared = Arc::clone(&shared);
        let owed = Arc::clone(&owed);
        let stop = Arc::clone(&stop);
        let watchers = Arc::clone(&watchers);
        let announced = Arc::clone(&announced);
        let dir = dir.clone();
        // A connection gets a thread. One client that asks for a snapshot, or
        // one delivery that is slow to run, cannot hold up the next hook.
        thread::spawn(move || {
            if serve(stream, &shared, &owed, &dir, &watchers, &announced) {
                // The other end is a different build of this program. Neither
                // end can read what the other one says. A daemon that stands
                // down leaves the name free for the daemon that the other end
                // expects, which is itself. The next event of any kind starts
                // that daemon. This daemon writes out what it applied first,
                // and the clients with it, so the daemon that takes over
                // inherits both.
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
        // The whole point of the pause: whatever refused it is still there. An
        // immediate retry saturates a core and recovers nothing.
        assert!(AFTER_REFUSAL >= Duration::from_millis(50));
    }

    #[test]
    fn every_change_made_before_a_delivery_is_covered_by_it() {
        // The whole point of an owed state rather than a delivery: the clients
        // are sent the state entire. Three changes during one delivery are one
        // delivery afterwards rather than three.
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
        // This rule exists for one failure. A sidebar is alive, and the
        // daemon draws to it. The multiplexer will not let go of the program
        // that draws to it. A count of those deliveries drops the client after three
        // of them, and nothing registers it again. The sidebar then shows
        // whatever it last received for as long as the session lasts.
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
        // The other half of it: a session that went away answers at once and
        // says so. That is the answer that a client is retired for.
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
        // It is a client that took the state, so the count towards its
        // retirement starts again, like any delivery that landed. Each run is
        // one short of the count that retires the client. If the run in between
        // forgave what came before it, the two runs stay two apart.
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
        // A daemon that refuses to carry on here is alive, reachable, and
        // permanently unable to answer anything.
        assert!(held(&shared).clients().is_empty());
    }
}
