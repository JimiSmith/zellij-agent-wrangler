//! The daemon: one per user, started by whichever hook first finds none
//! running.
//!
//! One connection is one thread, and the state behind one lock, because the
//! traffic is a handful of messages a second and a lock is far less machinery
//! than a core loop with a channel. Delivery is the one thing done outside the
//! lock: it runs a program per client, and holding the state while that happens
//! would stall every other connection behind it.

pub mod persist;
pub mod sink;
pub mod state;

use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, Listener, ListenerOptions, Stream};

use agent_wrangler_core::agent::FORMAT;

use crate::daemon::state::{Real, State};
use crate::paths;
use crate::proto::{read_message, write_message, Inbound, Outbound};

/// How often every held transcript is looked at again, and every held pid.
///
/// A second is what the eye reads as immediate for something that is not a
/// keystroke, and one stat per session at that rate costs nothing measurable.
const POLL: Duration = Duration::from_secs(1);

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
/// that died: a name that answers has an owner, and a name that does not is
/// taken back rather than treated as fatal.
fn bind() -> std::io::Result<Bound> {
    let name = paths::socket_name();
    let ns = name.clone().to_ns_name::<GenericNamespaced>()?;
    if Stream::connect(ns.clone()).is_ok() {
        return Ok(Bound::Taken);
    }
    ListenerOptions::new()
        .name(ns)
        .create_sync()
        .map(Bound::Ours)
}

/// Send one payload to every client, dropping the ones that have gone.
///
/// Side effect: runs a program per zellij client. Called with no lock held.
fn broadcast(shared: &Arc<Mutex<State>>) {
    let (payload, sinks) = {
        let state = shared.lock().expect("state lock");
        (state.payload(), state.sinks())
    };
    let mut gone = Vec::new();
    for sink in sinks {
        if !sink::deliver(&sink, &payload).sent() {
            gone.push(sink);
        }
    }
    if !gone.is_empty() {
        let mut state = shared.lock().expect("state lock");
        for sink in &gone {
            state.drop_sink(sink);
        }
    }
}

/// Deliver to one client, whatever the rest of them are doing.
fn deliver_one(shared: &Arc<Mutex<State>>, to: &crate::proto::Sink) {
    let payload = shared.lock().expect("state lock").payload();
    if !sink::deliver(to, &payload).sent() {
        shared.lock().expect("state lock").drop_sink(to);
    }
}

/// Read one connection to its end, applying what it says.
///
/// Returns `true` when the sender spoke a record format this build does not,
/// which is the one condition that makes the daemon stand down.
fn serve(stream: Stream, shared: &Arc<Mutex<State>>, dir: &Path) -> bool {
    let mut reader = BufReader::new(&stream);
    let mut stale = false;
    let mut changed = false;
    let mut announce: Option<crate::proto::Sink> = None;

    while let Ok(Some(message)) = read_message::<_, Inbound>(&mut reader) {
        match message {
            Inbound::Hook { format, .. } | Inbound::Register { format, .. } if format != FORMAT => {
                stale = true;
                break;
            }
            Inbound::Hook { hook, .. } => {
                let mut state = shared.lock().expect("state lock");
                changed |= state.on_hook(&hook, &Real);
            }
            Inbound::Register { sink, .. } => {
                shared.lock().expect("state lock").register(sink.clone());
                announce = Some(sink);
            }
            Inbound::Seen { session } => {
                let mut state = shared.lock().expect("state lock");
                changed |= state.on_seen(&session);
            }
            Inbound::Snapshot => {
                let payload = shared.lock().expect("state lock").payload();
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

    if changed {
        let saved = shared.lock().expect("state lock").snapshot();
        persist::save(dir, &saved);
        broadcast(shared);
    } else if let Some(sink) = announce {
        // A client that has just registered has nothing yet, so it is told the
        // state whether or not anything changed.
        deliver_one(shared, &sink);
    }
    stale
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
    initial.restore(persist::load(&dir), &Real);
    let shared = Arc::new(Mutex::new(initial));
    let stop = Arc::new(AtomicBool::new(false));

    {
        let shared = Arc::clone(&shared);
        let stop = Arc::clone(&stop);
        let dir = dir.clone();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(POLL);
                let changed = {
                    let mut state = shared.lock().expect("state lock");
                    state.poll(&Real)
                };
                if changed {
                    let saved = shared.lock().expect("state lock").snapshot();
                    persist::save(&dir, &saved);
                    broadcast(&shared);
                }
            }
        });
    }

    for incoming in listener.incoming() {
        let Ok(stream) = incoming else { continue };
        let shared = Arc::clone(&shared);
        let stop = Arc::clone(&stop);
        let dir = dir.clone();
        // A connection gets a thread, so one client asking for a snapshot, or
        // one delivery that is slow to run, cannot hold up the next hook.
        thread::spawn(move || {
            if serve(stream, &shared, &dir) {
                // The other end is a different build of this program, so what it
                // says cannot be read reliably and what this says cannot be read
                // by it. Standing down leaves the name free for the daemon it
                // expects, which is itself; the next event of any kind starts
                // that one.
                stop.store(true, Ordering::Relaxed);
                let saved = shared.lock().expect("state lock").snapshot();
                persist::save(&dir, &saved);
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
}
