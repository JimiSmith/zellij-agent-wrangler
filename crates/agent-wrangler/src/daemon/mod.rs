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
//! delivery is one delivery after it. The number of writes is therefore a
//! function of the delivery, and not of the burst of events from a busy
//! agent.

pub mod notify;
pub mod persist;
pub mod sink;
pub mod state;
pub mod watch;

use std::collections::BTreeSet;
use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, Listener, ListenerOptions, Stream};

use agent_wrangler_core::agent::FORMAT;
use agent_wrangler_core::notify::Notifier;

use crate::daemon::notify::Announced;
use crate::daemon::sink::{Delivery, Transports};
use crate::daemon::state::{look, read_hook, Call, Client, Real, State};
use crate::daemon::watch::Watchers;
use crate::paths;
use crate::proto::{read_message, write_message, Inbound, Outbound, Sink, Told, What};

/// How often the daemon looks at every held transcript again, and at every
/// held pid.
///
/// A second is what the eye reads as immediate for something that is not a
/// keystroke. One stat per session at that rate costs nothing measurable.
const POLL: Duration = Duration::from_secs(1);

/// How often the daemon writes down a held pipe while a call waits for the
/// user.
///
/// A client speaks only while it handles a message, so this is how often it
/// gets the chance. After the user answers a call, no other sidebar draws that
/// call for longer than this.
const SPEAK_WHILE_CALLING: Duration = Duration::from_secs(1);

/// How often the daemon writes down a held pipe when no call waits.
///
/// A client rarely has anything to say at these moments, so the beat is slow.
/// It does not stop altogether, because the daemon cannot know what a client
/// holds. Each write costs a line in the log of the multiplexer. That log rolls
/// at a fixed size, so a fast beat for no reason pushes out the records that a
/// person came to read.
const SPEAK_WHEN_QUIET: Duration = Duration::from_secs(30);

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

    /// This method waits until something is owed, or until `patience` runs out,
    /// and takes whatever it finds. It returns whether anything was owed.
    ///
    /// The wait ends either way, because the thread that delivers also writes
    /// the beat that lets a client speak. A quiet machine needs that beat as
    /// much as a busy one does.
    fn take(&self, patience: Duration) -> bool {
        let mut owed = self.owed.lock().unwrap_or_else(|e| e.into_inner());
        let until = Instant::now() + patience;
        while !*owed {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            let (waited, _) = self
                .told
                .wait_timeout(owed, left)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            owed = waited;
        }
        *owed = false;
        true
    }
}

/// This function claims one name, or finds that someone else holds it. `None`
/// says that something answered the name, so it is not this process's to take.
///
/// A connection attempt first tells a live listener from a name that a dead one
/// left behind. To take the name over is then safe *because* of that. Nothing
/// answered the name, so nothing listens on it. Without this attempt, a daemon
/// that was killed outright leaves a name that nothing can bind again. This is
/// true on every system where the name is a file rather than a name that the
/// kernel drops with the process.
///
/// The daemon's own socket and every socket sink are claimed here, so there is
/// one account of what a name that is already answered means.
pub(crate) fn claim(name: &str) -> std::io::Result<Option<Listener>> {
    let ns = name.to_ns_name::<GenericNamespaced>()?;
    if Stream::connect(ns.clone()).is_ok() {
        return Ok(None);
    }
    ListenerOptions::new()
        .name(ns)
        .try_overwrite(true)
        .create_sync()
        .map(Some)
}

/// This function writes the state out and tells every client. It drops the
/// clients that went away.
///
/// Side effect: it writes a file, and queues one write for each client. Nothing
/// here waits for a client to take its payload, so a publish costs the same
/// whatever the clients do.
///
/// The outcomes are those of the deliveries before this one, because a write
/// runs on a thread of its own. A client that went away is therefore retired a
/// publish later than it refused. It is retired either way, and no delivery
/// waits for that to be settled.
///
/// The caller holds no lock. This function takes the lock only to read what to
/// send, and to note the clients that it did not reach.
fn publish(
    shared: &Arc<Mutex<State>>,
    dir: &Path,
    watchers: &Watchers,
    transports: &mut Transports,
) {
    let (payload, clients, saved) = {
        let state = held(shared);
        (state.payload(), state.clients(), state.snapshot())
    };
    let agents = saved.len();
    persist::save(dir, &saved, &clients);
    for client in &clients {
        watchers.saw(|| What::Delivering {
            sink: client.sink.clone(),
            agents,
        });
        sink::deliver(transports, &client.sink, &payload);
    }
    let outcomes = transports.outcomes();
    for (sink, delivery) in &outcomes {
        if *delivery == Delivery::Failed {
            watchers.saw(|| What::Failed { sink: sink.clone() });
        }
    }
    let (retired, live) = {
        let mut state = held(shared);
        let retired: Vec<Sink> = outcomes
            .into_iter()
            .filter(|(sink, delivery)| answered(&mut state, sink, *delivery))
            .map(|(sink, _)| sink)
            .collect();
        (retired, live(&state))
    };
    for sink in retired {
        watchers.saw(|| What::Retired { sink: sink.clone() });
    }
    // The pipe process does not exit when its session dies, and a socket name
    // that nothing releases is a name that a later client cannot bind. A client
    // that this daemon just retired leaves both behind, unless this closes them.
    transports.retain(&live);
}

/// Every client that the daemon still delivers to.
fn live(state: &State) -> BTreeSet<Sink> {
    state
        .clients()
        .into_iter()
        .map(|client| client.sink)
        .collect()
}

/// This function gives up on every socket sink that has had no peer for long
/// enough, and tells the watchers.
///
/// The daemon reads its own listener, so this question costs nothing. It is
/// asked on every turn of the delivery loop, which is at most a second apart. A
/// turn with nothing to retire takes no lock at all.
///
/// Side effect: this function writes the state file, and releases a socket name.
fn retire_idle(
    shared: &Arc<Mutex<State>>,
    dir: &Path,
    watchers: &Watchers,
    transports: &mut Transports,
    now: Instant,
) {
    let stale = transports.stale(now);
    if stale.is_empty() {
        return;
    }
    let live = {
        let mut state = held(shared);
        for sink in &stale {
            if state.retire(sink) {
                watchers.saw(|| What::Retired { sink: sink.clone() });
            }
        }
        live(&state)
    };
    record(shared, dir);
    transports.retain(&live);
}

/// What one delivery says about the client it was for. This function returns
/// whether the daemon gave the client up.
///
/// Only a refusal counts against a zellij client. A refusal is the answer that a
/// client that went away gives. A pipe into a session that is not there exits
/// within milliseconds and says so.
///
/// Nothing counts against a socket sink. The exit status of a process was always
/// a poor measure of liveness: it reads a busy multiplexer as a refusal, and it
/// never finds a session that lives on with no sidebar in it. A socket answers
/// the question directly instead. The daemon reads its own listener, and a sink
/// with no peer for long enough is one to give up on.
fn answered(state: &mut State, sink: &Sink, delivery: Delivery) -> bool {
    if matches!(sink, Sink::Socket { .. }) {
        return false;
    }
    match delivery {
        Delivery::Sent => {
            state.reached(sink);
            false
        }
        Delivery::Failed => state.missed(sink),
    }
}

/// This function records that the user reached a session that called for them.
///
/// A client says this on the socket, or on the transport that the daemon
/// already holds open to it. Both arrive here, so there is one account of what
/// the message means.
fn seen(shared: &Arc<Mutex<State>>, owed: &Owed, watchers: &Watchers, session: &str) {
    let told = held(shared).on_seen(session);
    watchers.saw(|| What::Seen {
        session: session.to_string(),
        told,
    });
    if told {
        owed.owe();
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
            Inbound::Seen { session } => seen(shared, owed, watchers, &session),
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
    let Some(listener) = claim(&paths::socket_name())? else {
        return Ok(());
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
    // The restored clients are owed the state before anything changes. A socket
    // sink is bound by the delivery that goes to it, so without this a restored
    // one stays unbound until the next event, and its client finds nothing to
    // connect to.
    owed.owe();
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

    // What the clients say back, on the transports that the daemon holds open
    // to them. Every held transport reads on a thread of its own, and hands
    // over what it read. This thread is the one place that acts on a message
    // about an agent.
    let (told, heard) = channel();
    {
        let shared = Arc::clone(&shared);
        let owed = Arc::clone(&owed);
        let watchers = Arc::clone(&watchers);
        thread::spawn(move || {
            while let Ok(told) = heard.recv() {
                match told {
                    Told::Seen { session } => seen(&shared, &owed, &watchers, &session),
                    // A beat says only that the client is there. This thread
                    // acts on a message about an agent, and a beat is not one.
                    // The arm is empty because there is nothing left to do,
                    // and not because something is unfinished.
                    Told::Beat => {}
                }
            }
        });
    }

    // One thread makes every delivery. However many events arrive during one
    // delivery, they are one delivery afterwards rather than one delivery each.
    // This thread owns the held transports. Nothing else in the daemon
    // touches them.
    {
        let shared = Arc::clone(&shared);
        let owed = Arc::clone(&owed);
        let watchers = Arc::clone(&watchers);
        let dir = dir.clone();
        thread::spawn(move || {
            let mut transports = Transports::new(told);
            let mut spoke = Instant::now();
            loop {
                if owed.take(SPEAK_WHILE_CALLING) {
                    publish(&shared, &dir, &watchers, &mut transports);
                    spoke = Instant::now();
                } else if held(&shared).anyone_calling() || spoke.elapsed() >= SPEAK_WHEN_QUIET {
                    // Nothing changed, so nothing was written. A client speaks
                    // only while it handles a message. The one thing it says is
                    // that a call was answered. The beat is therefore fast while
                    // a call waits, and slow the rest of the time.
                    transports.nudge();
                    spoke = Instant::now();
                }
                retire_idle(&shared, &dir, &watchers, &mut transports, Instant::now());
            }
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
    fn a_call_is_listened_for_closely_and_silence_is_not() {
        // Both ends of the choice. The fast beat is what a person reads as
        // immediate when they answer a call. The slow beat is paid for in the
        // log of the multiplexer. That log rolls at a fixed size, so a fast
        // beat for no reason pushes out what somebody came to read.
        assert!(SPEAK_WHILE_CALLING <= Duration::from_secs(1));
        assert!(SPEAK_WHEN_QUIET >= SPEAK_WHILE_CALLING * 10);
    }

    #[test]
    fn nothing_is_owed_a_word_until_an_agent_wants_the_user() {
        let mut state = State::default();
        assert!(!state.anyone_calling());
        state.register(sidebar());
        assert!(!state.anyone_calling(), "a client is not a call");
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
        assert!(owed.take(Duration::from_secs(5)));

        let waiting = Arc::clone(&owed);
        let (delivered, heard) = std::sync::mpsc::channel();
        thread::spawn(move || {
            while !waiting.take(Duration::from_millis(20)) {}
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
    fn a_client_that_keeps_refusing_is_still_given_up_on() {
        // The other half of it: a session that went away answers at once and
        // says so. That is the answer that a client is retired for.
        let mut state = State::default();
        let client = sidebar();
        state.register(client.clone());
        for _ in 1..REFUSALS {
            assert!(!answered(&mut state, &client.sink, Delivery::Failed));
        }
        assert!(answered(&mut state, &client.sink, Delivery::Failed));
        assert!(state.clients().is_empty());
    }

    #[test]
    fn a_delivery_that_landed_forgives_the_refusals_before_it() {
        // Each run is one short of the count that retires the client. If the
        // delivery in between forgave what came before it, the two runs stay
        // two apart.
        let mut state = State::default();
        let client = sidebar();
        state.register(client.clone());
        for _ in 1..REFUSALS {
            answered(&mut state, &client.sink, Delivery::Failed);
        }
        answered(&mut state, &client.sink, Delivery::Sent);
        for _ in 1..REFUSALS {
            answered(&mut state, &client.sink, Delivery::Failed);
        }
        assert_eq!(state.clients(), vec![client]);
    }

    fn native() -> Client {
        Client {
            sink: Sink::Socket {
                name: "wrangler-tmux-work.sock".to_string(),
            },
            notify: None,
        }
    }

    #[test]
    fn every_kind_of_client_holds_a_transport_that_must_be_closed() {
        // What decides which transports survive a publish. A socket name that
        // nothing releases is a name that a later client cannot bind, exactly
        // as a pipe process that nothing kills outlives its session.
        let mut state = State::default();
        state.register(sidebar());
        state.register(native());
        assert_eq!(
            live(&state),
            BTreeSet::from([sidebar().sink, native().sink])
        );
    }

    #[test]
    fn a_retired_client_leaves_no_transport_behind() {
        // The pipe process does not exit when its session dies. A daemon that
        // only forgets the client leaves that process for as long as the user
        // stays logged in.
        let mut state = State::default();
        let client = sidebar();
        state.register(client.clone());
        assert!(!live(&state).is_empty());
        for _ in 0..REFUSALS {
            answered(&mut state, &client.sink, Delivery::Failed);
        }
        assert!(
            live(&state).is_empty(),
            "nothing keeps this session's pipe open"
        );
    }

    #[test]
    fn no_delivery_outcome_ever_retires_a_socket_sink() {
        // A socket sink is given up on for having no peer, and for nothing
        // else. A name that could not be bound is reported so that a watcher
        // sees it, and the next publish tries the name again.
        let mut state = State::default();
        let client = native();
        state.register(client.clone());
        for _ in 0..REFUSALS * 3 {
            assert!(!answered(&mut state, &client.sink, Delivery::Failed));
        }
        assert_eq!(state.clients(), vec![client]);
    }

    #[test]
    fn a_socket_sink_with_no_peer_is_given_up_on() {
        let mut state = State::default();
        let client = native();
        state.register(client.clone());
        assert!(state.retire(&client.sink));
        assert!(state.clients().is_empty());
        assert!(!state.retire(&client.sink), "there is nothing left to drop");
    }

    #[test]
    fn a_socket_sink_that_never_had_a_peer_is_retired_and_gives_up_its_name() {
        // The whole rule, from the bound name to the client that goes. A
        // sidebar that the user turned off leaves the session alive and the
        // socket empty, and the daemon must stop writing to it.
        let dir =
            std::env::temp_dir().join(format!("agent-wrangler-retire-{}", std::process::id()));
        let name = format!("agent-wrangler-retire-test-{}.sock", std::process::id());
        let shared = Arc::new(Mutex::new(State::default()));
        let watchers = Watchers::default();
        let (told, _heard) = channel();
        let mut transports = Transports::new(told);
        let client = Client {
            sink: Sink::Socket { name: name.clone() },
            notify: None,
        };
        held(&shared).register(client.clone());
        publish(&shared, &dir, &watchers, &mut transports);

        let now = Instant::now();
        retire_idle(&shared, &dir, &watchers, &mut transports, now);
        assert_eq!(
            held(&shared).clients(),
            vec![client],
            "it was only just bound"
        );

        retire_idle(
            &shared,
            &dir,
            &watchers,
            &mut transports,
            now + sink::NO_PEER,
        );
        assert!(held(&shared).clients().is_empty());
        // The name is released, so a later client can bind it.
        let (told, _heard) = channel();
        let mut after = Transports::new(told);
        let until = Instant::now() + Duration::from_secs(5);
        while after.stale(Instant::now() + sink::NO_PEER).is_empty() && Instant::now() < until {
            sink::deliver(
                &mut after,
                &Sink::Socket { name: name.clone() },
                "wrangler 3",
            );
        }
        assert_eq!(
            after.stale(Instant::now() + sink::NO_PEER),
            vec![Sink::Socket { name }],
            "the name was free to bind again"
        );
        after.retain(&BTreeSet::new());
        let _ = std::fs::remove_dir_all(&dir);
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
