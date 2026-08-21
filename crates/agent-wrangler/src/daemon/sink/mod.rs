//! This module delivers one state change to one client, and reads what a client
//! says back.
//!
//! A sink says how to reach a client, and says nothing about what the client is.
//! A new multiplexer therefore costs one variant and one arm in this module.
//! Nothing else learns about the new multiplexer.
//!
//! There are two transports, and they have one shape. A zellij client is reached
//! through one `zellij pipe` that stays open, because a wasm plugin cannot hold
//! a connection and the daemon must reach out to it. A native client holds a
//! connection itself, so the daemon binds a socket and the client reads it. Each
//! one carries the state out and the messages back. They differ in the pipe
//! under them and in nothing else.
//!
//! Nothing in this module waits on a delivery. Each transport has a writer of
//! its own, and a slot that holds one payload. A caller fills the slot and
//! returns. A client whose buffer is full therefore delays that client and no
//! other.

mod slot;
mod socket;
mod zellij;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Read};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use agent_wrangler_core::agent::flatten;

use crate::proto::{read_message, Sink, Told};

/// How long a socket sink may have no peer at all before the daemon gives up on
/// the client.
///
/// The end of a stream says that a peer has gone. A sink with no peer is not a
/// client that has gone, because the sidebars of a session come and go while the
/// session stays. So the daemon waits. This is long enough that a sidebar
/// restarting, or a daemon restarting and its clients connecting again, never
/// loses a registration. It is short enough that a session where the user turned
/// the sidebar off stops being written to within half a minute.
pub const NO_PEER: Duration = Duration::from_secs(30);

/// The outcome of one delivery.
///
/// A zellij client that this module cannot reach is a client that went away.
/// That outcome decides whether the daemon keeps the sink. A socket sink answers
/// the same question a better way, so the only outcome it reports is a name that
/// could not be bound. This type is deliberately not an error type. There is
/// nothing to report, and nobody to report it to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    Failed,
}

/// Every transport that the daemon holds open, keyed by the client it reaches.
///
/// One thread owns this. A transport is threads and sometimes a process, and
/// nothing else in the daemon has a reason to name one.
pub struct Transports {
    zellij: BTreeMap<String, zellij::Held>,
    sockets: BTreeMap<String, socket::Bound>,
    /// Every socket name that could not be bound, and when it was first
    /// refused.
    ///
    /// A name that something else answers is left alone, and the next publish
    /// tries it again. Without this clock those attempts never end, and a client
    /// that this daemon can never serve stays registered for as long as the
    /// daemon runs. A name that cannot be bound has no peer either, so the same
    /// wait retires it.
    refused: BTreeMap<String, Instant>,
    /// What the writers report, and the end that a caller drains.
    reported: Sender<(Sink, Delivery)>,
    outcomes: Receiver<(Sink, Delivery)>,
    /// Where a message from a client goes. The readers all share this end.
    told: Sender<Told>,
}

impl Transports {
    pub fn new(told: Sender<Told>) -> Self {
        let (reported, outcomes) = channel();
        Transports {
            zellij: BTreeMap::new(),
            sockets: BTreeMap::new(),
            refused: BTreeMap::new(),
            reported,
            outcomes,
            told,
        }
    }

    /// Queues one payload for one zellij session, and opens the pipe if none is
    /// open.
    ///
    /// Side effect: this method can spawn a process. It never waits for one. A
    /// spawn that fails counts as a failed delivery. A session that has gone
    /// answers in exactly that way.
    fn zellij(&mut self, session: &str, payload: &str) {
        // The question costs one call that does not wait, and it is asked here
        // rather than left to the writer. A writer learns that its child died
        // only from the write that failed, which is one publish after this one.
        // The sidebar then misses every state until the publish after that.
        if self.zellij.get(session).is_some_and(|held| !held.alive()) {
            if let Some(dead) = self.zellij.remove(session) {
                zellij::shut(&dead);
            }
        }
        if !self.zellij.contains_key(session) {
            let Some(held) = zellij::open(session, &self.reported, &self.told) else {
                self.report(
                    &Sink::Zellij {
                        session: session.to_string(),
                    },
                    Delivery::Failed,
                );
                return;
            };
            self.zellij.insert(session.to_string(), held);
        }
        self.zellij[session].fill(flatten(payload));
    }

    /// Queues one payload for one socket name, and binds the name if it is not
    /// bound.
    ///
    /// Side effect: this method can bind a name and spawn a thread. A name that
    /// something else answers is left alone, and counts as a failed delivery so
    /// that a watcher sees it. The next publish tries the name again.
    ///
    /// A delivery that reached the peers reports nothing. No count decides
    /// whether the daemon keeps a socket sink, so there would be nobody to tell.
    fn socket(&mut self, name: &str, payload: &str) {
        if !self.sockets.contains_key(name) {
            let Some(bound) = socket::bind(name, &self.told) else {
                self.refused
                    .entry(name.to_string())
                    .or_insert_with(Instant::now);
                self.report(
                    &Sink::Socket {
                        name: name.to_string(),
                    },
                    Delivery::Failed,
                );
                return;
            };
            self.refused.remove(name);
            self.sockets.insert(name.to_string(), bound);
        }
        self.sockets[name].fill(flatten(payload));
    }

    /// Records one outcome that this thread already knows.
    fn report(&self, sink: &Sink, delivery: Delivery) {
        let _ = self.reported.send((sink.clone(), delivery));
    }

    /// Every delivery outcome reported since the last call to this method.
    ///
    /// Side effect: a failed delivery closes the pipe that it was for. A child
    /// that died is therefore replaced by the next delivery to that session,
    /// rather than written to for as long as the daemon runs.
    pub fn outcomes(&mut self) -> Vec<(Sink, Delivery)> {
        let outcomes: Vec<(Sink, Delivery)> = self.outcomes.try_iter().collect();
        for (sink, delivery) in &outcomes {
            if let (Sink::Zellij { session }, Delivery::Failed) = (sink, delivery) {
                if let Some(held) = self.zellij.remove(session) {
                    zellij::shut(&held);
                }
            }
        }
        outcomes
    }

    /// Gives the plugins on every held pipe a message, so that they can speak.
    ///
    /// A plugin writes on a pipe only while it handles a message from that
    /// pipe. Zellij holds anything it writes at any other moment, and hands it
    /// over on the next message. Without this, a sidebar that answered a call
    /// holds that answer until something else changes, and nothing else
    /// changes, because the answer is the change.
    ///
    /// Side effect: this method writes one empty line to each held pipe. The
    /// line is a message to zellij and nothing to a sidebar, which reads no
    /// state in it and draws nothing again.
    ///
    /// This method passes over a pipe whose process ended. Only a delivery
    /// opens a pipe again, because only a delivery knows that the session is
    /// still a client.
    ///
    /// A socket needs none of this. A peer holds its connection open and reads
    /// whenever it likes, so it has a turn at every moment.
    pub fn nudge(&mut self) {
        for held in self.zellij.values() {
            if held.alive() {
                held.nudge();
            }
        }
    }

    /// Every socket sink that has had no peer for long enough to give up on.
    ///
    /// This method takes no lock of the daemon's, and reads a map with one entry
    /// per client. The delivery thread can therefore ask on every turn of its
    /// loop.
    pub fn stale(&self, now: Instant) -> Vec<Sink> {
        let bound = self
            .sockets
            .iter()
            .filter(|(_, bound)| bound.stale(now))
            .map(|(name, _)| name);
        let refused = self
            .refused
            .iter()
            .filter(|(_, since)| now.duration_since(**since) >= NO_PEER)
            .map(|(name, _)| name);
        bound
            .chain(refused)
            .map(|name| Sink::Socket { name: name.clone() })
            .collect()
    }

    /// Closes every transport whose client is no longer a client.
    ///
    /// Side effect: this method kills a process and waits for it, and releases a
    /// socket name. A pipe process does not exit when its session is killed, and
    /// a name that nothing releases is a name that a later client cannot bind. A
    /// daemon that only forgets the client leaves both behind, for as long as
    /// the user stays logged in.
    pub fn retain(&mut self, live: &BTreeSet<Sink>) {
        self.zellij.retain(|session, held| {
            if live.contains(&Sink::Zellij {
                session: session.clone(),
            }) {
                return true;
            }
            zellij::shut(held);
            false
        });
        self.sockets.retain(|name, bound| {
            if live.contains(&Sink::Socket { name: name.clone() }) {
                return true;
            }
            socket::shut(bound);
            false
        });
        self.refused
            .retain(|name, _| live.contains(&Sink::Socket { name: name.clone() }));
    }
}

/// Reads what the clients on one transport say, until the stream ends.
///
/// The end of the stream is the end of this thread. Nothing else stops it, and
/// nothing needs to. A killed child closes its stdout, and a peer that goes
/// closes its socket.
fn read_until_ended<R: Read>(reader: R, told: &Sender<Told>) {
    let mut reader = BufReader::new(reader);
    while let Ok(Some(message)) = read_message::<_, Told>(&mut reader) {
        if told.send(message).is_err() {
            return;
        }
    }
}

/// Hands one payload to one client.
///
/// Side effect: this function queues a write, and can start the transport that
/// carries it. Neither one waits for the client to take the payload. The outcome
/// arrives later, on [`Transports::outcomes`].
pub fn deliver(transports: &mut Transports, sink: &Sink, payload: &str) {
    match sink {
        Sink::Zellij { session } => transports.zellij(session, payload),
        Sink::Socket { name } => transports.socket(name, payload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transports() -> Transports {
        let (told, _heard) = channel();
        Transports::new(told)
    }

    fn name(what: &str) -> String {
        format!("agent-wrangler-test-{}-{what}.sock", std::process::id())
    }

    #[test]
    fn a_socket_delivery_that_landed_reports_nothing() {
        // No count decides whether the daemon keeps a socket sink, so there is
        // nobody to tell that a delivery landed.
        let mut transports = transports();
        let sink = Sink::Socket {
            name: name("delivery"),
        };
        deliver(&mut transports, &sink, "wrangler 3\nfirst");
        assert_eq!(transports.outcomes(), Vec::new());
        transports.retain(&BTreeSet::new());
    }

    #[test]
    fn a_name_that_cannot_be_bound_is_a_failed_delivery() {
        // Nothing retires the client for it. The record is what makes a name
        // that the daemon cannot hold visible to whoever is watching.
        let mut transports = transports();
        let taken = name("cannot-bind");
        let (told, _heard) = channel();
        let held = socket::bind(&taken, &told).expect("a bound name");
        let sink = Sink::Socket { name: taken };
        deliver(&mut transports, &sink, "wrangler 3");
        assert_eq!(transports.outcomes(), vec![(sink, Delivery::Failed)]);
        socket::shut(&held);
    }

    #[test]
    fn a_run_of_records_reaches_a_peer_as_one_line() {
        // The transport frames by the line, and every payload holds newlines
        // because the record separator is one. A peer takes one state per read
        // only because the breaks travel as something else.
        use agent_wrangler_core::agent::BREAK;
        use interprocess::local_socket::prelude::*;
        use interprocess::local_socket::{GenericNamespaced, Stream};
        use std::io::BufRead;

        let mut transports = transports();
        let taken = name("one-line");
        let sink = Sink::Socket {
            name: taken.clone(),
        };
        deliver(&mut transports, &sink, "first\nsecond");
        let ns = taken.to_ns_name::<GenericNamespaced>().expect("a name");
        let peer = Stream::connect(ns).expect("a peer");
        let mut line = String::new();
        BufReader::new(&peer).read_line(&mut line).expect("a state");
        assert!(line.ends_with('\n'), "one line: {line:?}");
        assert_eq!(line.trim_end(), format!("first{BREAK}second"));
        transports.retain(&BTreeSet::new());
    }

    #[test]
    fn a_client_that_is_no_longer_a_client_gives_up_its_name() {
        let mut transports = transports();
        let taken = name("released");
        let sink = Sink::Socket {
            name: taken.clone(),
        };
        deliver(&mut transports, &sink, "wrangler 3");
        transports.retain(&BTreeSet::from([sink.clone()]));
        assert!(transports.sockets.contains_key(&taken), "still a client");
        transports.retain(&BTreeSet::new());
        assert!(transports.sockets.is_empty());
    }

    #[test]
    fn a_name_that_stays_refused_is_given_up_on_like_one_with_no_peer() {
        // A name that this daemon can never bind is a client it can never
        // serve. Without the clock the attempts never end, and a `Failed`
        // record is written for it on every publish for as long as the daemon
        // runs.
        let mut transports = transports();
        let taken = name("stays-refused");
        let (told, _heard) = channel();
        let held = socket::bind(&taken, &told).expect("a bound name");
        let sink = Sink::Socket {
            name: taken.clone(),
        };
        deliver(&mut transports, &sink, "wrangler 3");
        let now = Instant::now();
        assert_eq!(
            transports.stale(now),
            Vec::new(),
            "it was only just refused"
        );
        assert_eq!(transports.stale(now + NO_PEER), vec![sink.clone()]);
        // A name that answers later is not one that was refused.
        socket::shut(&held);
        let until = Instant::now() + std::time::Duration::from_secs(5);
        while !transports.sockets.contains_key(&taken) && Instant::now() < until {
            deliver(&mut transports, &sink, "wrangler 3");
        }
        assert!(
            transports.sockets.contains_key(&taken),
            "it bound in the end"
        );
        assert!(transports.refused.is_empty());
        transports.retain(&BTreeSet::new());
    }

    #[test]
    fn a_sink_with_no_peer_is_reported_stale_and_one_just_bound_is_not() {
        let mut transports = transports();
        let sink = Sink::Socket {
            name: name("stale-report"),
        };
        deliver(&mut transports, &sink, "wrangler 3");
        let now = Instant::now();
        assert_eq!(transports.stale(now), Vec::new());
        assert_eq!(transports.stale(now + NO_PEER), vec![sink]);
        transports.retain(&BTreeSet::new());
    }
}
