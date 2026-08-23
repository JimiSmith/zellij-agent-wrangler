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

use agent_wrangler_core::agent::escape_record_breaks;

use crate::proto::{read_message, Sink, Told};

/// Every transport that the daemon holds open, keyed by the client it reaches.
///
/// One thread owns this. A transport is threads and sometimes a process, and
/// nothing else in the daemon has a reason to name one.
pub struct Transports {
    zellij: BTreeMap<String, zellij::Held>,
    sockets: BTreeMap<String, socket::Bound>,
    /// Every delivery that did not land, and the end that a caller drains.
    ///
    /// This channel carries failures alone. A delivery that landed says nothing
    /// about whether the client behind it works, so there is nobody to tell. A
    /// client is kept for as long as it speaks, and the lines it speaks arrive
    /// on `told`.
    reported: Sender<Sink>,
    failures: Receiver<Sink>,
    /// Where a line from a client goes, with the client that said it. The
    /// readers all share this end.
    told: Sender<(Sink, Told)>,
}

impl Transports {
    pub fn new(told: Sender<(Sink, Told)>) -> Self {
        let (reported, failures) = channel();
        Transports {
            zellij: BTreeMap::new(),
            sockets: BTreeMap::new(),
            reported,
            failures,
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
                self.report(&Sink::Zellij {
                    session: session.to_string(),
                });
                return;
            };
            self.zellij.insert(session.to_string(), held);
        }
        self.zellij[session].fill(escape_record_breaks(payload));
    }

    /// Queues one payload for one socket name, and binds the name if it is not
    /// bound.
    ///
    /// Side effect: this method can bind a name and spawn a thread. A name that
    /// something else answers is left alone, and counts as a delivery that did
    /// not land so that a watcher sees it. The next publish tries the name
    /// again. Nothing retires the client for it. A client that this daemon can
    /// never bind a name for never speaks either, and silence is what retires
    /// it.
    ///
    /// A delivery that reached the peers reports nothing. No delivery decides
    /// whether the daemon keeps a client, so there would be nobody to tell.
    fn socket(&mut self, name: &str, payload: &str) {
        if !self.sockets.contains_key(name) {
            let Some(bound) = socket::bind(name, &self.told) else {
                self.report(&Sink::Socket {
                    name: name.to_string(),
                });
                return;
            };
            self.sockets.insert(name.to_string(), bound);
        }
        self.sockets[name].fill(escape_record_breaks(payload));
    }

    /// Records one delivery that did not land, and that this thread already
    /// knows about.
    fn report(&self, sink: &Sink) {
        let _ = self.reported.send(sink.clone());
    }

    /// Every delivery that did not land since the last call to this method.
    ///
    /// Side effect: a failed delivery closes the pipe that it was for. A child
    /// that died is therefore replaced by the next delivery to that session,
    /// rather than written to for as long as the daemon runs.
    pub fn failures(&mut self) -> Vec<Sink> {
        let failures: Vec<Sink> = self.failures.try_iter().collect();
        for sink in &failures {
            if let Sink::Zellij { session } = sink {
                if let Some(held) = self.zellij.remove(session) {
                    zellij::shut(&held);
                }
            }
        }
        failures
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
    }
}

/// Reads what the clients on one transport say, until the stream ends.
///
/// Each line carries the sink that it arrived on. The reader of a transport is
/// the only place that knows which client is at the other end, and the daemon
/// needs that to know which client spoke.
///
/// The end of the stream is the end of this thread. Nothing else stops it, and
/// nothing needs to. A killed child closes its stdout, and a peer that goes
/// closes its socket.
fn read_until_ended<R: Read>(sink: &Sink, reader: R, told: &Sender<(Sink, Told)>) {
    let mut reader = BufReader::new(reader);
    while let Ok(Some(message)) = read_message::<_, Told>(&mut reader) {
        if told.send((sink.clone(), message)).is_err() {
            return;
        }
    }
}

/// Hands one payload to one client.
///
/// Side effect: this function queues a write, and can start the transport that
/// carries it. Neither one waits for the client to take the payload. A write
/// that did not land arrives later, on [`Transports::failures`].
pub fn deliver(transports: &mut Transports, sink: &Sink, payload: &str) {
    match sink {
        Sink::Zellij { session } => transports.zellij(session, payload),
        Sink::Socket { name } => transports.socket(name, payload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn transports() -> Transports {
        let (told, _heard) = channel();
        Transports::new(told)
    }

    fn name(what: &str) -> String {
        format!("agent-wrangler-test-{}-{what}.sock", std::process::id())
    }

    #[test]
    fn a_socket_delivery_that_landed_reports_nothing() {
        // No delivery decides whether the daemon keeps a client, so there is
        // nobody to tell that a delivery landed.
        let mut transports = transports();
        let sink = Sink::Socket {
            name: name("delivery"),
        };
        deliver(&mut transports, &sink, "wrangler 3\nfirst");
        assert_eq!(transports.failures(), Vec::new());
        transports.retain(&BTreeSet::new());
    }

    #[test]
    fn a_name_that_cannot_be_bound_is_a_delivery_that_did_not_land() {
        // Nothing retires the client for it. The record makes a name that the
        // daemon cannot hold visible to whoever is watching. The client is
        // retired for saying nothing, which a client behind an unbound name
        // cannot help but do.
        let mut transports = transports();
        let taken = name("cannot-bind");
        let (told, _heard) = channel();
        let held = socket::bind(&taken, &told).expect("a bound name");
        let sink = Sink::Socket { name: taken };
        deliver(&mut transports, &sink, "wrangler 3");
        assert_eq!(transports.failures(), vec![sink]);
        socket::shut(&held);
    }

    #[test]
    fn a_run_of_records_reaches_a_peer_as_one_line() {
        // The transport frames by the line, and every payload holds newlines
        // because the record separator is one. A peer takes one state per read
        // only because the breaks travel as something else.
        use agent_wrangler_core::agent::ESCAPED_RECORD_BREAK;
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
        assert_eq!(
            line.trim_end(),
            format!("first{ESCAPED_RECORD_BREAK}second")
        );
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
    fn a_name_that_something_else_answers_is_bound_once_it_is_free() {
        // Nothing here retires the client for a name it cannot bind, so the
        // daemon must keep trying. A publish that gave up would leave a client
        // with a name that nothing ever binds again.
        let mut transports = transports();
        let taken = name("stays-refused");
        let (told, _heard) = channel();
        let held = socket::bind(&taken, &told).expect("a bound name");
        let sink = Sink::Socket {
            name: taken.clone(),
        };
        deliver(&mut transports, &sink, "wrangler 3");
        assert_eq!(transports.failures(), vec![sink.clone()]);
        socket::shut(&held);
        let until = Instant::now() + Duration::from_secs(5);
        while !transports.sockets.contains_key(&taken) && Instant::now() < until {
            deliver(&mut transports, &sink, "wrangler 3");
        }
        assert!(
            transports.sockets.contains_key(&taken),
            "it bound in the end"
        );
        transports.retain(&BTreeSet::new());
    }
}
