//! What stands in for a daemon in the tests of this crate.
//!
//! A test binds a name of its own and answers on it. Nothing here reaches the
//! real daemon, and nothing here needs one to run.

use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::Duration;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, Listener, ListenerOptions, Stream};

/// How long a test waits for something that must arrive.
///
/// A test that waits this long fails. Without the wait, a test that never
/// receives its line hangs, and a hung test costs the whole job.
pub const TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// A name of this run's own, so that two tests never share a socket.
pub fn unique_socket_name(what: &str) -> String {
    format!("tmux-wrangler-test-{}-{what}.sock", std::process::id())
}

/// Binds one name.
///
/// Side effect: this function binds a name. The name is released when the
/// listener is dropped.
pub fn bind_socket(name: &str) -> Listener {
    let ns = name.to_ns_name::<GenericNamespaced>().expect("a name");
    ListenerOptions::new()
        .name(ns)
        .create_sync()
        .expect("a listener")
}

/// The two ends of one connection.
pub struct ConnectedPair {
    /// Kept, so that the name stays bound while the test runs. Nothing reads
    /// it. Dropping it is what releases the name.
    _listener: Listener,
    /// The end that the code under test writes and reads.
    pub client_end: Stream,
    /// The end that stands in for the daemon.
    pub daemon_end: Stream,
}

impl ConnectedPair {
    /// Writes one line on the daemon end.
    pub fn send_line(&self, line: &str) {
        let mut writer: &Stream = &self.daemon_end;
        writeln!(writer, "{line}").expect("a line");
        writer.flush().expect("a line");
    }

    /// Drops the daemon end, and gives the client end back.
    ///
    /// Whatever was said stays readable. The reader finds the end of the stream
    /// after it.
    pub fn close_daemon_end(self) -> Stream {
        drop(self.daemon_end);
        self.client_end
    }
}

/// One connection, on a name of this test's own.
///
/// Side effect: this function binds a name and spawns a thread. The accept runs
/// on that thread, because a connect to a name that nobody accepts yet can be
/// refused outright on some systems.
pub fn connected_pair(what: &str) -> ConnectedPair {
    let name = unique_socket_name(what);
    let listener = bind_socket(&name);
    let accepting = thread::spawn(move || {
        let stream = listener.accept().expect("a peer");
        (listener, stream)
    });
    let ns = name
        .as_str()
        .to_ns_name::<GenericNamespaced>()
        .expect("a name");
    let client_end = Stream::connect(ns).expect("a connection");
    let (listener, daemon_end) = accepting.join().expect("the accept thread");
    ConnectedPair {
        _listener: listener,
        client_end,
        daemon_end,
    }
}

/// Every line that arrives on `stream`, read on a thread of its own.
///
/// Side effect: this function spawns a thread and takes the stream. A test reads
/// the lines with a wait, so a line that never arrives fails the test instead of
/// hanging it.
pub fn read_lines_on_thread(stream: Stream) -> Receiver<String> {
    let (sending, lines) = channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            if sending.send(line.trim_end().to_string()).is_err() {
                return;
            }
        }
    });
    lines
}
