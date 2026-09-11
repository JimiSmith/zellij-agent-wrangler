//! One connection's sole writer for heartbeats and client messages.
//!
//! A sidebar that only reads says nothing to the daemon. The heartbeat story
//! makes the daemon give up on a client that it has not heard from, so the
//! sidebar must say this instead.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;

use agent_wrangler_core::client_message::ClientMessage;
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
    /// A stop message wakes an idle writer without waiting for its interval.
    stop_sender: Sender<Option<ClientMessage>>,
    stopped: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl RunningHeartbeat {
    /// Stops the heartbeat, and waits for its thread to end.
    ///
    /// The flag discards queued messages before the next write. A stop message
    /// wakes an idle writer. An in-progress socket write must still finish.
    pub fn stop(self) {
        self.stopped.store(true, Ordering::Release);
        let _ = self.stop_sender.send(None);
        let _ = self.thread.join();
    }

    pub(crate) fn message_sender(&self) -> Sender<Option<ClientMessage>> {
        self.stop_sender.clone()
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
    let stopped = Arc::new(AtomicBool::new(false));
    let thread_stopped = Arc::clone(&stopped);
    let thread = thread::spawn(move || {
        write_heartbeats_until_stopped(&stream, &heartbeat, &wake, &thread_stopped)
    });
    RunningHeartbeat {
        stop_sender: stop,
        stopped,
        thread,
    }
}

/// Writes the heartbeat line until the heartbeat stops, or until the daemon stops taking
/// it.
///
/// The first heartbeat goes out at once. Each message restarts the interval. The
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
    wake: &Receiver<Option<ClientMessage>>,
    stopped: &AtomicBool,
) {
    let mut writer: &Stream = stream;
    let mut line = heartbeat.line.clone();
    loop {
        if stopped.load(Ordering::Acquire) {
            return;
        }
        if writeln!(writer, "{line}")
            .and_then(|()| writer.flush())
            .is_err()
        {
            return;
        }
        // Every message proves liveness, so only an idle connection needs a beat.
        line = match wake.recv_timeout(heartbeat.interval) {
            Ok(Some(message)) => message.encode(),
            Err(RecvTimeoutError::Timeout) => heartbeat.line.clone(),
            Ok(None) | Err(RecvTimeoutError::Disconnected) => return,
        };
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
