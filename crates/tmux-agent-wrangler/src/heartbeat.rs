//! The heartbeat that one connection writes for as long as it lasts.
//!
//! A sidebar that only reads says nothing to the daemon. The heartbeat story
//! makes the daemon give up on a client that it has not heard from, so the
//! sidebar must say this instead.

use std::io::Write;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use interprocess::local_socket::Stream;

/// What to write, and how often to write it.
///
/// Both are given to this module rather than held in it. The daemon and the
/// client must agree on the message and on the time, so the crate that both ends
/// share owns them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeartbeatSettings {
    /// The time between one heartbeat and the next.
    pub interval: Duration,
    /// The line to write, without the newline that frames it.
    pub line: String,
}

/// A heartbeat that runs.
pub struct RunningHeartbeat {
    /// The end that wakes the thread.
    ///
    /// The thread waits on the other end, so a drop of this wakes it at once. A
    /// thread that slept the whole interval instead would outlive its connection
    /// by up to that interval, and would hold the stream open for that long.
    stop_sender: Sender<()>,
    thread: JoinHandle<()>,
}

impl RunningHeartbeat {
    /// Stops the heartbeat, and waits for its thread to end.
    ///
    /// The thread wakes as soon as the sender is dropped. This function
    /// therefore returns in the time of one wake and not in the time of one
    /// interval.
    pub fn stop(self) {
        drop(self.stop_sender);
        let _ = self.thread.join();
    }
}

/// Starts a heartbeat on `stream`.
///
/// Side effect: this function spawns a thread. The thread holds the stream
/// through an `Arc`, which is what the transport's own documentation asks for. A
/// reference to a stream writes, and splitting the stream buys nothing.
pub fn start_heartbeat(stream: &Arc<Stream>, heartbeat: &HeartbeatSettings) -> RunningHeartbeat {
    let (stop, wake) = channel();
    let stream = Arc::clone(stream);
    let heartbeat = heartbeat.clone();
    let thread = thread::spawn(move || write_heartbeats_until_stopped(&stream, &heartbeat, &wake));
    RunningHeartbeat {
        stop_sender: stop,
        thread,
    }
}

/// Writes the heartbeat line until the heartbeat stops, or until the daemon stops taking
/// it.
///
/// The first heartbeat goes out at once, and the next one after the interval. The
/// immediate first heartbeat does two jobs, and anybody who wants to remove that line
/// must answer both.
///
/// One. Without it a client is silent for the register, the connect and one
/// whole interval added together. Each of those three can grow, so the margin is
/// arithmetic that nobody wrote down. The immediate heartbeat bounds the silence by
/// the interval alone, which is a property and not a margin.
///
/// Two. It protects a client that binds late. Under the heartbeat story the
/// daemon holds a clock for each client, and a client that connects to an entry
/// whose clock is nearly spent is retired before its first heartbeat. The immediate
/// heartbeat stamps that clock on connect, so it cannot be.
///
/// A write that fails ends this thread and nothing else. The failure says that
/// the daemon has gone, and the reader of the same stream says so as well.
fn write_heartbeats_until_stopped(
    stream: &Stream,
    heartbeat: &HeartbeatSettings,
    wake: &Receiver<()>,
) {
    let mut writer: &Stream = stream;
    loop {
        if writeln!(writer, "{}", heartbeat.line)
            .and_then(|()| writer.flush())
            .is_err()
        {
            return;
        }
        match wake.recv_timeout(heartbeat.interval) {
            // The wait ran out, so the next heartbeat is due.
            Err(RecvTimeoutError::Timeout) => {}
            // Anything else is the handle going away, which ends the heartbeat.
            _ => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_daemon;
    use std::time::Instant;

    /// An interval that no test can wait out. A heartbeat that arrives under one of
    /// these is the immediate first heartbeat and nothing else.
    const LONGER_THAN_ANY_TEST: Duration = Duration::from_secs(300);

    fn settings(interval: Duration) -> HeartbeatSettings {
        HeartbeatSettings {
            interval,
            line: r#"{"kind":"beat"}"#.to_string(),
        }
    }

    #[test]
    fn the_first_heartbeat_goes_out_at_once() {
        // The daemon knows nothing about a client that has just connected, and
        // on a reconnect the daemon is fresh. Under the heartbeat story, saying
        // so at once is what keeps the register, the connect and the interval
        // from adding up into one silence.
        let pair = test_daemon::connected_pair("first-beat");
        let heard = test_daemon::read_lines_on_thread(pair.daemon_end);
        let beating = start_heartbeat(&Arc::new(pair.client_end), &settings(LONGER_THAN_ANY_TEST));
        assert_eq!(
            heard.recv_timeout(test_daemon::TEST_TIMEOUT),
            Ok(r#"{"kind":"beat"}"#.to_string())
        );
        beating.stop();
    }

    #[test]
    fn the_heartbeat_goes_out_again_after_every_interval() {
        let pair = test_daemon::connected_pair("more-beats");
        let heard = test_daemon::read_lines_on_thread(pair.daemon_end);
        let beating = start_heartbeat(
            &Arc::new(pair.client_end),
            &settings(Duration::from_millis(20)),
        );
        for _ in 0..3 {
            assert_eq!(
                heard.recv_timeout(test_daemon::TEST_TIMEOUT),
                Ok(r#"{"kind":"beat"}"#.to_string())
            );
        }
        beating.stop();
    }

    #[test]
    fn stopping_the_heartbeat_does_not_wait_for_the_interval() {
        // A thread that slept the interval and then read a flag would take five
        // minutes to answer this test. It would also hold a dead stream open for
        // that long after every reconnect.
        let pair = test_daemon::connected_pair("stop-beat");
        let heard = test_daemon::read_lines_on_thread(pair.daemon_end);
        let beating = start_heartbeat(&Arc::new(pair.client_end), &settings(LONGER_THAN_ANY_TEST));
        heard
            .recv_timeout(test_daemon::TEST_TIMEOUT)
            .expect("the first beat");
        let at = Instant::now();
        beating.stop();
        assert!(
            at.elapsed() < test_daemon::TEST_TIMEOUT,
            "{:?}",
            at.elapsed()
        );
    }

    #[test]
    fn a_heartbeat_that_cannot_be_written_ends_its_thread() {
        // The daemon went away. The reader of the same stream reports that, and
        // this thread has nothing to add.
        let pair = test_daemon::connected_pair("dead-beat");
        drop(pair.daemon_end);
        let beating = start_heartbeat(&Arc::new(pair.client_end), &settings(LONGER_THAN_ANY_TEST));
        let at = Instant::now();
        beating.stop();
        assert!(
            at.elapsed() < test_daemon::TEST_TIMEOUT,
            "{:?}",
            at.elapsed()
        );
    }
}
