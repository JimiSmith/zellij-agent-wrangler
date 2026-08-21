//! One local socket per sink name, bound by the daemon and read by any number
//! of peers.
//!
//! A client that can hold a connection needs no command to reach it. It
//! connects, and it reads until the daemon or the client goes. One socket serves
//! one session, so every sidebar of that session reads the same one.
//!
//! The daemon holds the newest payload for each name. A client is owed the state
//! the moment it registers, which is before any peer of it connects. Without the
//! held payload the first deliveries would reach nobody. Every delivery carries
//! the whole state, so one held payload is enough.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, Listener, Stream};

use super::slot::Slot;
use super::NO_PEER;
use crate::proto::Told;

/// How long to wait after an accept that failed, before accepting again.
///
/// Whatever refused the connection is usually still there on the next call, so
/// an immediate retry is a spin rather than a retry.
const AFTER_REFUSAL: Duration = Duration::from_millis(100);

/// One bound name, and the peers that read it.
pub struct Bound {
    /// The name, kept so that the accept thread can be woken by a connection to
    /// it.
    name: String,
    /// Set when the daemon is finished with this name. The accept thread reads
    /// it after each accept.
    stop: Arc<AtomicBool>,
    peers: Arc<Peers>,
}

impl Bound {
    /// Queues one payload for every peer, and holds it for the peers that
    /// connect later.
    pub fn fill(&self, line: String) {
        self.peers.fill(line);
    }

    /// Whether this name has had no peer for longer than [`NO_PEER`].
    pub fn stale(&self, now: Instant) -> bool {
        self.peers
            .alone_since()
            .is_some_and(|since| now.duration_since(since) >= NO_PEER)
    }
}

/// Every peer on one name, the newest payload, and how long there has been
/// nobody.
struct Peers {
    fed: Mutex<Fed>,
}

#[derive(Default)]
struct Fed {
    /// The newest payload. A peer that connects after the state changed is
    /// written this before anything else.
    last: Option<String>,
    slots: BTreeMap<u64, Arc<Slot>>,
    /// The id of the next peer. Ids are never reused, so a peer that leaves
    /// cannot take a later peer's slot with it.
    next: u64,
    /// When the last peer left, or when the name was bound. `None` while a peer
    /// is connected.
    alone: Option<Instant>,
}

impl Peers {
    fn new(now: Instant) -> Self {
        Peers {
            fed: Mutex::new(Fed {
                alone: Some(now),
                ..Fed::default()
            }),
        }
    }

    /// Takes in one peer, and gives it the payload that is held.
    fn joined(&self) -> (u64, Arc<Slot>) {
        let mut fed = self.held();
        let id = fed.next;
        fed.next += 1;
        let slot = Arc::new(Slot::default());
        if let Some(last) = fed.last.clone() {
            slot.fill(last);
        }
        fed.slots.insert(id, Arc::clone(&slot));
        fed.alone = None;
        (id, slot)
    }

    /// Drops one peer and releases its writer. If it was the last peer, the
    /// clock that retires this sink starts now.
    fn left(&self, id: u64, now: Instant) {
        let mut fed = self.held();
        if let Some(slot) = fed.slots.remove(&id) {
            slot.close();
        }
        if fed.slots.is_empty() {
            fed.alone = Some(now);
        }
    }

    fn fill(&self, line: String) {
        let mut fed = self.held();
        for slot in fed.slots.values() {
            slot.fill(line.clone());
        }
        fed.last = Some(line);
    }

    fn alone_since(&self) -> Option<Instant> {
        self.held().alone
    }

    /// Releases every writer. The readers end when their peers disconnect.
    fn close_all(&self, now: Instant) {
        let mut fed = self.held();
        for (_, slot) in std::mem::take(&mut fed.slots) {
            slot.close();
        }
        fed.alone = Some(now);
    }

    fn held(&self) -> MutexGuard<'_, Fed> {
        self.fed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Binds one name and starts accepting on it.
///
/// Side effect: this function binds a name and spawns a thread. It answers
/// `None` for a name that something else already answers, and for one that
/// cannot be bound at all.
pub fn bind(name: &str, told: &Sender<Told>) -> Option<Bound> {
    let listener = super::super::claim(name).ok().flatten()?;
    let peers = Arc::new(Peers::new(Instant::now()));
    let stop = Arc::new(AtomicBool::new(false));

    let accepting = (Arc::clone(&peers), Arc::clone(&stop), told.clone());
    thread::spawn(move || {
        let (peers, stop, told) = accepting;
        accept_until_stopped(&listener, &peers, &stop, &told);
    });

    Some(Bound {
        name: name.to_string(),
        stop,
        peers,
    })
}

/// Releases one name and every peer on it.
///
/// Side effect: this function connects to the name once. The accept thread is
/// blocked inside `accept`, and a connection is the only thing that returns
/// from there. The thread then finds the flag, returns, and drops the listener,
/// which is what releases the name.
///
/// The reader of a peer that is still connected ends when that peer
/// disconnects. Nothing here shuts a peer's stream, because a retired sink is
/// one that had no peer in the first place.
pub fn shut(bound: &Bound) {
    bound.stop.store(true, Ordering::Relaxed);
    if let Ok(name) = bound.name.as_str().to_ns_name::<GenericNamespaced>() {
        let _ = Stream::connect(name);
    }
    bound.peers.close_all(Instant::now());
}

/// Accepts peers until the daemon says to stop.
fn accept_until_stopped(
    listener: &Listener,
    peers: &Arc<Peers>,
    stop: &AtomicBool,
    told: &Sender<Told>,
) {
    loop {
        let incoming = listener.accept();
        // The connection that woke this thread is read before it is answered.
        // It carries nothing, and the flag it was sent for is what matters.
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let Ok(stream) = incoming else {
            thread::sleep(AFTER_REFUSAL);
            continue;
        };
        joined(peers, stream, told);
    }
}

/// Gives one peer a writer and a reader.
///
/// Side effect: this function spawns two threads. Both hold the same stream
/// through an `Arc`, which is what the transport's own documentation asks for:
/// a reference to a stream reads and writes, and splitting it buys nothing.
fn joined(peers: &Arc<Peers>, stream: Stream, told: &Sender<Told>) {
    let stream = Arc::new(stream);
    let (id, slot) = peers.joined();

    let writing = Arc::clone(&stream);
    thread::spawn(move || write_until_closed(&writing, &slot));

    let peers = Arc::clone(peers);
    let told = told.clone();
    thread::spawn(move || {
        super::read_until_ended(&*stream, &told);
        // The end of the stream says that this peer has gone. That releases its
        // writer, and starts the clock if it was the last peer.
        peers.left(id, Instant::now());
    });
}

/// Writes each payload as one line, until the slot is closed or the peer stops
/// taking them.
///
/// A write that fails ends this thread and nothing else. What the failure means
/// is that the peer has gone, and the reader of that peer says so.
fn write_until_closed(stream: &Stream, slot: &Slot) {
    let mut writer: &Stream = stream;
    while let Some(line) = slot.take() {
        if writeln!(writer, "{line}")
            .and_then(|()| writer.flush())
            .is_err()
        {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc::channel;

    /// A name of this run's own, so that two tests never share a socket.
    fn name(what: &str) -> String {
        format!("agent-wrangler-test-{}-{what}.sock", std::process::id())
    }

    fn connect(name: &str) -> Stream {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let ns = name.to_ns_name::<GenericNamespaced>().expect("a name");
            match Stream::connect(ns) {
                Ok(stream) => return stream,
                Err(error) if Instant::now() >= until => panic!("no peer: {error}"),
                Err(_) => thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    fn line(stream: &Stream) -> String {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("a state");
        line.trim_end().to_string()
    }

    #[test]
    fn every_peer_on_one_name_receives_every_state() {
        let (told, _heard) = channel();
        let name = name("two-peers");
        let bound = bind(&name, &told).expect("a bound name");
        let one = connect(&name);
        let two = connect(&name);
        // Both peers are accepted before the fill, or the state is held for
        // whichever one arrived late. Either way both must receive it.
        let until = Instant::now() + Duration::from_secs(5);
        while bound.peers.held().slots.len() < 2 && Instant::now() < until {
            thread::sleep(Duration::from_millis(10));
        }
        bound.fill("wrangler 3".to_string());
        assert_eq!(line(&one), "wrangler 3");
        assert_eq!(line(&two), "wrangler 3");
        shut(&bound);
    }

    #[test]
    fn a_peer_that_arrives_late_receives_the_state_at_once() {
        // The daemon owes the state the moment a client registers, which is
        // before any peer of it connects. Without the held payload, the first
        // deliveries reach nobody at all.
        let (told, _heard) = channel();
        let name = name("late-peer");
        let bound = bind(&name, &told).expect("a bound name");
        bound.fill("wrangler 3".to_string());
        let peer = connect(&name);
        assert_eq!(line(&peer), "wrangler 3");
        shut(&bound);
    }

    #[test]
    fn a_peer_that_leaves_is_dropped_and_starts_the_clock() {
        let (told, _heard) = channel();
        let name = name("peer-leaves");
        let bound = bind(&name, &told).expect("a bound name");
        let peer = connect(&name);
        let until = Instant::now() + Duration::from_secs(5);
        while bound.peers.alone_since().is_some() && Instant::now() < until {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(bound.peers.alone_since(), None, "a peer is connected");
        drop(peer);
        let until = Instant::now() + Duration::from_secs(5);
        while bound.peers.alone_since().is_none() && Instant::now() < until {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            bound.peers.alone_since().is_some(),
            "the end of the stream says that the peer has gone"
        );
        assert!(
            bound.peers.held().slots.is_empty(),
            "its writer is released"
        );
        shut(&bound);
    }

    #[test]
    fn a_name_with_no_peer_for_long_enough_is_stale() {
        let (told, _heard) = channel();
        let name = name("stale");
        let bound = bind(&name, &told).expect("a bound name");
        let now = Instant::now();
        assert!(!bound.stale(now), "it was only just bound");
        assert!(bound.stale(now + NO_PEER));
        shut(&bound);
    }

    #[test]
    fn a_name_with_a_peer_is_never_stale() {
        let (told, _heard) = channel();
        let name = name("not-stale");
        let bound = bind(&name, &told).expect("a bound name");
        let _peer = connect(&name);
        let until = Instant::now() + Duration::from_secs(5);
        while bound.peers.alone_since().is_some() && Instant::now() < until {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!bound.stale(Instant::now() + NO_PEER * 10));
        shut(&bound);
    }

    #[test]
    fn a_name_that_something_else_answers_is_left_alone() {
        // Whoever holds the name keeps it. The daemon publishes nothing there
        // and tries again at the next publish.
        let (told, _heard) = channel();
        let name = name("taken");
        let first = bind(&name, &told).expect("a bound name");
        assert!(bind(&name, &told).is_none());
        shut(&first);
    }

    #[test]
    fn a_name_that_a_dead_daemon_left_behind_is_bound_again() {
        // The whole reason for connecting first. On a system where the name is
        // a file, a daemon that was killed outright leaves a name that nothing
        // answers, and nothing would ever bind it again.
        let (told, _heard) = channel();
        let name = name("left-behind");
        let dead = bind(&name, &told).expect("a bound name");
        shut(&dead);
        // The accept thread returns and drops the listener, which is what
        // releases the name.
        let until = Instant::now() + Duration::from_secs(5);
        let mut bound = bind(&name, &told);
        while bound.is_none() && Instant::now() < until {
            thread::sleep(Duration::from_millis(20));
            bound = bind(&name, &told);
        }
        let bound = bound.expect("the name was taken over");
        let peer = connect(&name);
        bound.fill("wrangler 3".to_string());
        assert_eq!(line(&peer), "wrangler 3");
        shut(&bound);
    }

    #[test]
    fn a_peer_reads_the_newest_state_and_not_a_queue_of_them() {
        let (told, _heard) = channel();
        let name = name("newest");
        let bound = bind(&name, &told).expect("a bound name");
        bound.fill("first".to_string());
        bound.fill("second".to_string());
        let peer = connect(&name);
        assert_eq!(line(&peer), "second");
        shut(&bound);
    }

    #[test]
    fn what_a_peer_says_reaches_the_daemon() {
        let (told, heard) = channel();
        let name = name("speaks");
        let bound = bind(&name, &told).expect("a bound name");
        let peer = connect(&name);
        let mut writer: &Stream = &peer;
        writeln!(writer, r#"{{"kind":"seen","session":"9f3c-1a"}}"#).expect("a message");
        writer.flush().expect("a message");
        assert_eq!(
            heard.recv_timeout(Duration::from_secs(5)),
            Ok(Told::Seen {
                session: "9f3c-1a".to_string()
            })
        );
        shut(&bound);
    }

    #[test]
    fn the_time_without_a_peer_is_longer_than_any_restart() {
        // A client that is retired goes deaf for good, because it registers
        // once. The wait must cover a sidebar restarting and a daemon restarting
        // with its clients connecting again.
        assert!(NO_PEER >= Duration::from_secs(30));
    }
}
