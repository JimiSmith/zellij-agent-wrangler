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
use interprocess::local_socket::{GenericNamespaced, Listener, Stream};

use agent_wrangler_core::agent::FORMAT;
use agent_wrangler_core::notify::Notifier;

use crate::daemon::notify::Announced;
use crate::daemon::sink::Transports;
use crate::daemon::state::{look, read_hook, Call, Client, Real, State};
use crate::daemon::watch::Watchers;
use crate::paths;
use crate::platform;
use crate::proto::{
    read_message, write_message, ClientMessage, DeliveryTarget, Inbound, MonitorEvent, Outbound,
};

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

/// This function claims one name, or finds that something else holds it.
/// `None` says that another process holds the name, so it is not this
/// process's to take.
///
/// The daemon's own socket and every socket sink are claimed here, so there is
/// one account of what a name that something else holds means.
///
/// Each system decides how to tell a held name from a free one, and
/// `platform::claim_socket_name` answers that once for each. A unix socket
/// outlives its process and a named pipe does not, so the two answers share no
/// code.
pub(crate) fn claim(name: &str) -> std::io::Result<Option<Listener>> {
    platform::claim_socket_name(name.to_ns_name::<GenericNamespaced>()?)
}

/// This function writes the state out and tells every client.
///
/// Side effect: it writes a file, and queues one write for each client. Nothing
/// here waits for a client to take its payload, so a publish costs the same
/// whatever the clients do.
///
/// No client leaves here. A delivery that did not land is written down for
/// whoever watches, and decides nothing. A client leaves when it stops speaking,
/// which [`retire_silent`] finds.
///
/// The failures are those of the deliveries before this one, because a write
/// runs on a thread of its own.
///
/// The caller holds no lock. This function takes the lock only to read what to
/// send.
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
        watchers.saw(|| MonitorEvent::Delivering {
            sink: client.sink.clone(),
            agents,
        });
        sink::deliver(transports, &client.sink, &payload);
    }
    for sink in transports.failures() {
        watchers.saw(|| MonitorEvent::Failed { sink: sink.clone() });
    }
    // The pipe process does not exit when its session dies, and a socket name
    // that nothing releases is a name that a later client cannot bind. A client
    // that the daemon retired leaves both behind, unless this closes them.
    let live = live(&held(shared));
    transports.retain(&live);
}

/// Every client that the daemon still delivers to.
fn live(state: &State) -> BTreeSet<DeliveryTarget> {
    state
        .clients()
        .into_iter()
        .map(|client| client.sink)
        .collect()
}

/// This function gives up on every client that has said nothing for long
/// enough, and tells the watchers.
///
/// One question retires a client of either kind: can it still send a message?
/// A client answers it by speaking. The exit status of a process was always a
/// poor measure. It reads a busy multiplexer as a refusal, and it never finds a
/// session that lives on with no sidebar in it. An open connection is a poor
/// measure as well. It says that the kernel kept the connection, and says
/// nothing about the process behind it.
///
/// This is asked on every turn of the delivery loop, which is at most a second
/// apart. It reads a map with one entry for each client, so the lock is held for
/// no longer than that read takes.
///
/// Side effect: this function writes the state file, and closes the transport of
/// every client that it retires.
fn retire_silent(
    shared: &Arc<Mutex<State>>,
    dir: &Path,
    watchers: &Watchers,
    transports: &mut Transports,
    now: Instant,
) {
    let silent = held(shared).silent(now);
    if silent.is_empty() {
        return;
    }
    let live = {
        let mut state = held(shared);
        for sink in &silent {
            if state.retire(sink) {
                watchers.saw(|| MonitorEvent::Retired { sink: sink.clone() });
            }
        }
        live(&state)
    };
    record(shared, dir);
    transports.retain(&live);
}

/// This function records that the user reached a session that called for them.
///
/// A client says this on the socket, or on the transport that the daemon
/// already holds open to it. Both arrive here, so there is one account of what
/// the message means.
fn seen(shared: &Arc<Mutex<State>>, owed: &Owed, watchers: &Watchers, session: &str) {
    let told = held(shared).on_seen(session);
    watchers.saw(|| MonitorEvent::Seen {
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
                watchers.saw(|| MonitorEvent::Hook {
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
                watchers.saw(|| MonitorEvent::Registered { sink: sink.clone() });
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
                watchers.saw(|| MonitorEvent::Asked);
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
    // to them. Every held transport reads on a thread of its own. Each one
    // hands what it read to this thread, so what a message means is written
    // once.
    let (told, heard) = channel();
    {
        let shared = Arc::clone(&shared);
        let owed = Arc::clone(&owed);
        let watchers = Arc::clone(&watchers);
        thread::spawn(move || {
            while let Ok((sink, told)) = heard.recv() {
                // Any line keeps the client. The line says that the client can
                // still send a message, whatever else it says. A client with
                // something to report therefore sends no separate beat.
                held(&shared).spoke(&sink, Instant::now());
                match told {
                    ClientMessage::Seen { session } => seen(&shared, &owed, &watchers, &session),
                    // A beat says only that the client is there, and the line
                    // above already recorded that. What is left is to make the
                    // beat visible to whoever watches, because a client that
                    // stops beating is retired and nothing else explains why.
                    ClientMessage::Beat => {
                        watchers.saw(|| MonitorEvent::Beat { sink: sink.clone() })
                    }
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
                retire_silent(&shared, &dir, &watchers, &mut transports, Instant::now());
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
    use crate::daemon::state::SILENCE;

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
            sink: DeliveryTarget::Zellij {
                session: "proto".to_string(),
            },
            notify: None,
        }
    }

    #[test]
    fn a_busy_multiplexer_never_retires_a_live_client() {
        // The whole reason for the rule. A refused delivery says that this
        // publish did not land. A sidebar that answers is working, whatever the
        // exit status of a pipe process said a moment earlier.
        let mut state = State::default();
        let client = sidebar();
        state.register(client.clone());
        let now = Instant::now();
        state.spoke(&client.sink, now);
        assert!(state.silent(now + SILENCE / 2).is_empty());
        assert_eq!(state.clients(), vec![client]);
    }

    fn native() -> Client {
        Client {
            sink: DeliveryTarget::Socket {
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
        assert!(state.retire(&client.sink));
        assert!(
            live(&state).is_empty(),
            "nothing keeps this session's pipe open"
        );
    }

    #[test]
    fn a_client_of_either_kind_is_retired_by_the_same_silence() {
        // One rule covers both transports. A zellij session with no sidebar
        // left in it answers no message, and a socket peer that stopped
        // draining writes no line. Neither of them speaks.
        let mut state = State::default();
        state.register(sidebar());
        state.register(native());
        let now = Instant::now();
        assert!(state.silent(now).is_empty(), "they only just registered");
        assert_eq!(
            state.silent(now + SILENCE),
            vec![sidebar().sink, native().sink]
        );
    }

    #[test]
    fn a_client_that_left_is_dropped_once_and_not_twice() {
        let mut state = State::default();
        let client = native();
        state.register(client.clone());
        assert!(state.retire(&client.sink));
        assert!(state.clients().is_empty());
        assert!(!state.retire(&client.sink), "there is nothing left to drop");
    }

    #[test]
    fn a_client_that_says_nothing_is_retired_and_gives_up_its_name() {
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
            sink: DeliveryTarget::Socket { name: name.clone() },
            notify: None,
        };
        held(&shared).register(client.clone());
        publish(&shared, &dir, &watchers, &mut transports);

        let now = Instant::now();
        retire_silent(&shared, &dir, &watchers, &mut transports, now);
        assert_eq!(
            held(&shared).clients(),
            vec![client],
            "it only just registered"
        );

        retire_silent(&shared, &dir, &watchers, &mut transports, now + SILENCE);
        assert!(held(&shared).clients().is_empty());

        // The name is released, so a later client can bind it.
        let (told, _heard) = channel();
        let mut after = Transports::new(told);
        let sink = DeliveryTarget::Socket { name };
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            sink::deliver(&mut after, &sink, "wrangler 3");
            if after.failures().is_empty() {
                break;
            }
            assert!(
                Instant::now() < until,
                "the name was never free to bind again"
            );
        }
        after.retain(&BTreeSet::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_peer_that_holds_its_connection_open_and_says_nothing_is_retired() {
        // The whole reason for the rule. An open connection says that the
        // kernel kept it, and says nothing about the process behind it. A
        // sidebar whose reader thread died holds one open for as long as the
        // process lives, and draws nothing with what arrives on it.
        let dir =
            std::env::temp_dir().join(format!("agent-wrangler-wedged-{}", std::process::id()));
        let name = format!("agent-wrangler-wedged-test-{}.sock", std::process::id());
        let shared = Arc::new(Mutex::new(State::default()));
        let watchers = Watchers::default();
        let (told, _heard) = channel();
        let mut transports = Transports::new(told);
        let client = Client {
            sink: DeliveryTarget::Socket { name: name.clone() },
            notify: None,
        };
        held(&shared).register(client);
        publish(&shared, &dir, &watchers, &mut transports);

        let ns = name
            .as_str()
            .to_ns_name::<GenericNamespaced>()
            .expect("a name");
        let until = Instant::now() + Duration::from_secs(5);
        let peer = loop {
            match Stream::connect(ns.clone()) {
                Ok(stream) => break stream,
                Err(error) => assert!(Instant::now() < until, "no peer: {error}"),
            }
        };

        retire_silent(
            &shared,
            &dir,
            &watchers,
            &mut transports,
            Instant::now() + SILENCE,
        );
        assert!(
            held(&shared).clients().is_empty(),
            "the peer was connected the whole time and never said a word"
        );
        drop(peer);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // This test holds the name with a listener of its own, and accepts nothing
    // on it. Every probe after the first one therefore meets a name with no
    // free connection. On Windows that case made `claim` wait for ever. The
    // probe now gives up at once, and reads the refusal as "something holds
    // it", which is the answer that this test needs.
    #[test]
    fn a_run_of_deliveries_that_did_not_land_never_retires_a_client() {
        // A delivery that did not land says nothing about the client. The old
        // rule counted refusals and gave up after three, so a busy multiplexer
        // answered exactly like a client that went away.
        let dir =
            std::env::temp_dir().join(format!("agent-wrangler-refused-{}", std::process::id()));
        let name = format!("agent-wrangler-refused-test-{}.sock", std::process::id());
        // Something else answers the name, so the daemon binds nothing and no
        // delivery lands.
        let taken = claim(&name)
            .expect("a name")
            .expect("nothing else holds it");
        let shared = Arc::new(Mutex::new(State::default()));
        let watchers = Watchers::default();
        let (told, _heard) = channel();
        let mut transports = Transports::new(told);
        let client = Client {
            sink: DeliveryTarget::Socket { name },
            notify: None,
        };
        held(&shared).register(client.clone());
        for _ in 0..5 {
            publish(&shared, &dir, &watchers, &mut transports);
        }
        assert_eq!(held(&shared).clients(), vec![client]);
        drop(taken);
        transports.retain(&BTreeSet::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_that_a_listener_holds_is_claimed_by_nobody_else() {
        let name = format!("agent-wrangler-claim-test-{}.sock", std::process::id());
        let held = claim(&name).expect("a name").expect("nothing holds it yet");
        assert!(
            claim(&name).expect("a name").is_none(),
            "a listener holds the name, so a second claim takes nothing"
        );
        drop(held);
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
