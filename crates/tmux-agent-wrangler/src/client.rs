//! Registering the sink, connecting to the socket that the daemon binds, and writing
//! out every state that arrives.

use std::io::{BufRead, BufReader, Read};
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, Stream};

use crate::heartbeat::{self, HeartbeatSettings};
use crate::socket_name::SocketName;
use crate::tmux_location::TmuxLocation;
use crate::FatalError;

/// The program that registers this client with the daemon.
///
/// The program runs by name, so the system resolves it. This story assumes that
/// it is on the path.
const REGISTER_PROGRAM: &str = "agent-wrangler";

/// How long this program tries a socket, as a number of tries and the pause
/// between them.
///
/// The daemon binds the name while it handles the registration, which is after
/// this program sent it. A short gap is therefore expected. The wait is bounded,
/// because a daemon that never binds the name is a fault to report rather than a
/// thing to wait for.
const CONNECT_ATTEMPTS: u32 = 40;
const PAUSE_BETWEEN_ATTEMPTS: Duration = Duration::from_millis(50);

/// Which end of this program stopped.
///
/// A read that fails and a write that fails are two outcomes and not one. The
/// kind of an error names what went wrong, and it does not name which end it
/// went wrong at. So this answer is a type. Nothing in this module reads
/// `ErrorKind` to tell the two apart.
///
/// The caller cannot reconnect against a reader that has gone, because this type
/// offers no such path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionEnd {
    /// The daemon went away. The caller registers again and connects again.
    DaemonDisconnected,
    /// The reader of this program's output went away. The caller stops.
    OutputClosed,
}

/// Makes one attempt to connect to a socket name.
fn connect_once(name: &SocketName) -> std::io::Result<Stream> {
    let ns = name.as_str().to_ns_name::<GenericNamespaced>()?;
    Stream::connect(ns)
}

/// Connects to the socket, and tries again while the daemon binds it.
///
/// This function keeps the last error. A caller that gives up therefore reports
/// why the last try failed, rather than reporting only that it gave up.
fn connect_with_retry(name: &SocketName) -> std::io::Result<Stream> {
    let mut last = None;
    for try_number in 0..CONNECT_ATTEMPTS {
        // The pause comes before every try except the first. A pause after the
        // last try would only delay the message that says this program gave up.
        if try_number > 0 {
            thread::sleep(PAUSE_BETWEEN_ATTEMPTS);
        }
        match connect_once(name) {
            Ok(stream) => return Ok(stream),
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("nothing bound the socket")))
}

/// The word that names this kind of client to the client program.
///
/// The daemon reaches this client by binding a socket and writing on it, and
/// this word asks the daemon to do that. The client program accepts a set of
/// these words, and a word outside that set is refused with an exit status and
/// nothing drawn.
const REGISTER_KIND: &str = "socket";

/// The flag that hands the desktop notification command to the daemon.
const NOTIFY_FLAG: &str = "--notify";

/// The command that registers this client with the daemon.
///
/// The words are built here and run by the caller. A test can therefore read the
/// program and its arguments on any system, with the program installed or not.
/// These words are a contract with another program, and a mistake in them
/// compiles and passes every test that does not read them.
fn build_register_command(name: &SocketName, notifier: &[String]) -> Command {
    let mut command = Command::new(REGISTER_PROGRAM);
    command.args(["register", REGISTER_KIND, name.as_str()]);
    if !notifier.is_empty() {
        command.arg(NOTIFY_FLAG);
        command.args(notifier);
    }
    command
}

/// Registers the socket sink with the daemon.
///
/// Side effect: this function runs `agent-wrangler register socket <name>`. That
/// command starts a daemon if none runs, and the daemon binds the name while it
/// handles the registration. A registration is idempotent by name, so the second
/// sidebar of one session changes nothing.
///
/// This is the one path that registers a tmux client, and it runs again on
/// every round of the reconnect loop above. A daemon that gave up on a client,
/// or a daemon that restarted, has dropped the client record and released the
/// socket name. Only a new registration makes it bind the name again.
fn register_with_daemon(name: &SocketName, notifier: &[String]) -> Result<(), FatalError> {
    let status = build_register_command(name, notifier)
        .status()
        .map_err(FatalError::RegisterDidNotRun)?;
    match status.success() {
        true => Ok(()),
        false => Err(FatalError::RegisterFailed(status)),
    }
}

/// Hands over every payload that arrives, until one end or the other stops.
///
/// A read that fails ends this function in the same way as the end of the
/// stream. Both say that the daemon has gone, and the caller answers both the
/// same way: it registers again and connects again.
///
/// `take` receives one payload, exactly as it came off the socket. An error
/// from `take` says that the reader of these payloads has stopped. A stopped
/// reader is not a lost daemon, so this function reconnects to nothing and
/// reports nothing.
///
/// The reader is taken by value rather than as a stream, so a test can give a
/// reader that fails. Without that, no test reaches the failing arm below,
/// because dropping either end of a local socket is a clean close on both
/// systems.
fn read_until_connection_ends<R: Read, T: FnMut(&str) -> std::io::Result<()>>(
    reader: R,
    mut take: T,
) -> ConnectionEnd {
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            // These two are ONE arm on purpose. The two systems spell the end of
            // a stream differently: a unix peer's read answers zero after a
            // shutdown, and a Windows client's read FAILS after a
            // `DisconnectNamedPipe`. A program that waited for zero alone would
            // work on unix and wait for ever on Windows.
            Ok(0) | Err(_) => return ConnectionEnd::DaemonDisconnected,
            Ok(_) => {}
        }
        if take(&line).is_err() {
            return ConnectionEnd::OutputClosed;
        }
    }
}

/// Reads one connection until it ends, and heartbeats on it for as long as it
/// lasts.
///
/// Every connection beats. The daemon gives up on a client that says nothing,
/// and a client that only reads says nothing at all.
pub fn read_one_connection<T: FnMut(&str) -> std::io::Result<()>>(
    stream: Stream,
    take: T,
    heartbeat: &HeartbeatSettings,
) -> ConnectionEnd {
    let stream = Arc::new(stream);
    let beating = heartbeat::start_heartbeat(&stream, heartbeat);
    // The deref is explicit because the reader is now generic, and a generic
    // parameter takes no deref coercion. `&Stream` is what reads.
    let ended = read_until_connection_ends(&*stream, take);
    beating.stop();
    ended
}

/// Registers, connects and serves, over and over, until something stops this
/// program.
///
/// The registration comes BEFORE the connection, and that order is load bearing.
/// A daemon that gives up on a client releases the socket name and drops the
/// client record together, so only a new registration makes the daemon bind the
/// name again. A program that reconnected alone would find nothing to connect
/// to, and would stay deaf.
///
/// This loop goes round when the stream ends. Whether a retired client's stream
/// ends is the daemon's side of the contract and not this program's.
fn reconnect_loop<T, N, R, C>(
    mut take: T,
    heartbeat: &HeartbeatSettings,
    mut name: N,
    mut register: R,
    mut connect: C,
) -> Result<(), FatalError>
where
    T: FnMut(&str) -> std::io::Result<()>,
    N: FnMut() -> Result<SocketName, FatalError>,
    R: FnMut(&SocketName) -> Result<(), FatalError>,
    C: FnMut(&SocketName) -> Result<Stream, FatalError>,
{
    loop {
        let name = name()?;
        register(&name)?;
        let stream = connect(&name)?;
        match read_one_connection(stream, &mut take, heartbeat) {
            ConnectionEnd::DaemonDisconnected => {}
            ConnectionEnd::OutputClosed => return Ok(()),
        }
    }
}

/// Registers, connects, and writes out the state until something stops this
/// program.
///
/// Side effect: this function runs `tmux` and `agent-wrangler`, and it hands
/// every payload to `take`. It returns when `take` fails, and it answers an
/// error when it gives up on the daemon.
///
/// `notifier` is the command that raises a desktop notification, and it is
/// empty when the user asked for none. The registration carries it, because the
/// daemon raises the notification and this program never does.
pub fn run_client<T: FnMut(&str) -> std::io::Result<()>>(
    take: T,
    heartbeat: &HeartbeatSettings,
    notifier: &[String],
) -> Result<(), FatalError> {
    let location = TmuxLocation::from_environment()?;
    reconnect_loop(
        take,
        heartbeat,
        // The session is read again on every round. A window that moved to
        // another session therefore names the right socket as soon as the daemon
        // blinks. It costs one process for each round.
        || {
            Ok(SocketName::new(
                location.server_socket(),
                &location.read_session()?,
            ))
        },
        |name| register_with_daemon(name, notifier),
        |name| {
            connect_with_retry(name).map_err(|why| FatalError::SocketNeverBound {
                name: name.as_str().to_string(),
                why,
            })
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_daemon;
    use crate::tmux_location::TmuxSessionId;
    use agent_wrangler_core::agent;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Instant;

    /// A socket name of this run's own, built the way that the program builds
    /// one. Nothing else on the machine derives the same name.
    fn test_socket_name(what: &str) -> SocketName {
        let server = format!("tmux-wrangler-test-{}-{what}", std::process::id());
        SocketName::new(&server, &TmuxSessionId::new("$3").expect("a session"))
    }

    /// The program that a command runs, as text.
    fn command_program(command: &Command) -> String {
        command.get_program().to_string_lossy().into_owned()
    }

    /// The arguments of a command, as text.
    fn command_args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    /// One payload, framed as the daemon frames it.
    fn payload(records: &str) -> String {
        agent::escape_record_breaks(&agent::build_state_message(records))
    }

    /// The header that every payload leads with.
    fn header() -> String {
        format!("wrangler {}", agent::FORMAT)
    }

    /// A sink that keeps every payload, in the way that the sidebar keeps one.
    /// It keeps a payload whole and splits none, because the reader hands one
    /// line over exactly as it came off the socket.
    fn keep_payloads(kept: &mut Vec<String>) -> impl FnMut(&str) -> std::io::Result<()> + '_ {
        move |payload| {
            kept.push(payload.to_string());
            Ok(())
        }
    }

    #[test]
    fn every_state_that_arrives_is_handed_over_whole() {
        // One line off the socket is one payload. The reader never splits it.
        // The record breaks inside it travel escaped, and only whatever reads
        // the records undoes them.
        let pair = test_daemon::connected_pair("every-state");
        pair.send_line(&payload("recA\nrecB"));
        pair.send_line(&payload(""));
        let mut kept = Vec::new();
        assert_eq!(
            read_one_connection(
                pair.close_daemon_end(),
                keep_payloads(&mut kept),
                &test_heartbeat()
            ),
            ConnectionEnd::DaemonDisconnected
        );
        assert_eq!(kept.len(), 2);
        assert_eq!(
            agent::restore_record_breaks(&kept[0]).trim_end(),
            format!("{}\nrecA\nrecB", header())
        );
        assert_eq!(agent::restore_record_breaks(&kept[1]).trim_end(), header());
    }

    #[test]
    fn the_end_of_the_stream_says_that_the_daemon_went() {
        let pair = test_daemon::connected_pair("stream-ends");
        let mut kept = Vec::new();
        assert_eq!(
            read_one_connection(
                pair.close_daemon_end(),
                keep_payloads(&mut kept),
                &test_heartbeat()
            ),
            ConnectionEnd::DaemonDisconnected
        );
        assert!(kept.is_empty());
    }

    #[test]
    fn a_read_that_fails_says_the_daemon_went_just_as_the_end_of_the_stream_does() {
        // The two systems spell the end of a stream differently. A unix peer's
        // read answers zero after a shutdown, and a Windows client's read fails
        // after a `DisconnectNamedPipe`. This test covers the failing spelling,
        // which no socket in these tests can produce: dropping either end is a
        // clean close on both systems.
        struct FailingReader;
        impl Read for FailingReader {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::from(std::io::ErrorKind::ConnectionReset))
            }
        }
        let mut kept = Vec::new();
        assert_eq!(
            read_until_connection_ends(FailingReader, keep_payloads(&mut kept)),
            ConnectionEnd::DaemonDisconnected
        );
        assert!(kept.is_empty());
    }

    #[test]
    fn a_sink_that_went_away_is_not_a_daemon_that_went_away() {
        // The two failures are told apart by the type and never by the kind of
        // the error. Only one of them reconnects. The sink of the sidebar fails
        // when the thread that draws has stopped, and nothing must reconnect
        // for that.
        let pair = test_daemon::connected_pair("reader-went");
        pair.send_line(&payload("recA"));
        assert_eq!(
            read_one_connection(
                pair.close_daemon_end(),
                |_| Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
                &test_heartbeat()
            ),
            ConnectionEnd::OutputClosed
        );
    }

    /// A heartbeat that no test waits out. Every test here is about something
    /// else, and a beat that arrives during one must not be read as its
    /// subject.
    fn test_heartbeat() -> HeartbeatSettings {
        HeartbeatSettings {
            interval: Duration::from_secs(300),
            line: agent_wrangler_core::client_message::ClientMessage::Beat.encode(),
        }
    }

    #[test]
    fn one_connection_sends_heartbeats_while_it_lasts() {
        let pair = test_daemon::connected_pair("serve-beats");
        let heard = test_daemon::read_lines_on_thread(pair.daemon_end);
        let settings = HeartbeatSettings {
            interval: Duration::from_millis(20),
            line: "beat".to_string(),
        };
        let serving = thread::spawn(move || {
            let mut kept = Vec::new();
            read_one_connection(pair.client_end, keep_payloads(&mut kept), &settings)
        });
        assert_eq!(
            heard.recv_timeout(test_daemon::TEST_TIMEOUT),
            Ok("beat".to_string())
        );
        // Dropping this end stops the reader, which drops the daemon end, which
        // ends the connection that `read_one_connection` holds.
        drop(heard);
        assert_eq!(
            serving.join().expect("the serving thread"),
            ConnectionEnd::DaemonDisconnected
        );
    }

    #[test]
    fn losing_the_stream_registers_again_before_it_connects_again() {
        // The order is the whole of this test. A daemon that gave up on this
        // client released the name and dropped the client record together, so a
        // program that only reconnected would find nothing bound and would stay
        // deaf. Two counters would pass while the crate connected first.
        let done: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let rounds_left = RefCell::new(2);

        let naming = Rc::clone(&done);
        let name = || {
            // The loop ends only on an error, so the third round answers one.
            // Which error it is does not matter here.
            if *rounds_left.borrow() == 0 {
                return Err(FatalError::NotInsideTmux);
            }
            *rounds_left.borrow_mut() -= 1;
            naming.borrow_mut().push("name");
            Ok(test_socket_name("rounds"))
        };

        let registering = Rc::clone(&done);
        let register = |_: &SocketName| {
            registering.borrow_mut().push("register");
            Ok(())
        };

        let connecting = Rc::clone(&done);
        let connect = |_: &SocketName| {
            connecting.borrow_mut().push("connect");
            // A connection that ends at once, so the round ends and the loop
            // goes round again.
            Ok(test_daemon::connected_pair("rounds").close_daemon_end())
        };

        let mut kept = Vec::new();
        assert!(reconnect_loop(
            keep_payloads(&mut kept),
            &test_heartbeat(),
            name,
            register,
            connect
        )
        .is_err());
        assert_eq!(
            *done.borrow(),
            ["name", "register", "connect", "name", "register", "connect"]
        );
    }

    #[test]
    fn a_socket_that_something_bound_is_connected_to_at_once() {
        let sink = test_socket_name("reachable");
        let _listener = test_daemon::bind_socket(sink.as_str());
        let at = Instant::now();
        assert!(connect_with_retry(&sink).is_ok());
        assert!(
            at.elapsed() < test_daemon::TEST_TIMEOUT,
            "{:?}",
            at.elapsed()
        );
    }

    #[test]
    fn a_socket_that_nothing_bound_is_given_up_on() {
        let sink = test_socket_name("unbound");
        let at = Instant::now();
        assert!(connect_with_retry(&sink).is_err());
        // The rule is that the retry ends. `connect_with_retry` sleeps between
        // its tries, so the ceiling is that sleep and the wait of a test on top
        // of it. Every other test of this binary runs beside this one, and a
        // ceiling that leaves out the sleep fails on a loaded machine.
        assert!(
            at.elapsed() < CONNECT_ATTEMPTS * PAUSE_BETWEEN_ATTEMPTS + test_daemon::TEST_TIMEOUT,
            "{:?}",
            at.elapsed()
        );
    }

    #[test]
    fn the_register_command_names_the_program_the_kind_and_the_name() {
        // These words are the contract with `agent-wrangler`. A mistake in them
        // compiles, and it fails only against a real daemon.
        let sink = test_socket_name("words");
        let command = build_register_command(&sink, &[]);
        assert_eq!(command_program(&command), "agent-wrangler");
        assert_eq!(
            command_args(&command),
            ["register", "socket", sink.as_str()]
        );
    }

    #[test]
    fn the_register_command_hands_over_the_desktop_notifier() {
        // The daemon raises the desktop notification, once per call however
        // many sidebars hold it. The daemon learns the command from the
        // registration, so a notifier that stayed in this program would raise
        // nothing at all.
        let sink = test_socket_name("notifier");
        let notifier = ["notify-send".to_string(), "--urgency".to_string()];
        let command = build_register_command(&sink, &notifier);
        assert_eq!(
            command_args(&command),
            [
                "register",
                "socket",
                sink.as_str(),
                "--notify",
                "notify-send",
                "--urgency"
            ]
        );
    }

    #[test]
    fn giving_up_takes_a_bounded_time() {
        // The daemon gives up on a client that said nothing for ninety seconds.
        // A reconnect must end well inside that time. A slower reconnect costs
        // the registration that it tries to restore, because a client registers
        // once and a client that the daemon dropped goes deaf.
        //
        // The daemon owns the ninety seconds and this crate cannot read it. A
        // shared constant for it would put a daemon rule in a client, so this
        // bound is written down instead. Three seconds is safe for any silence
        // longer than about ten.
        assert!(PAUSE_BETWEEN_ATTEMPTS * CONNECT_ATTEMPTS <= Duration::from_secs(3));
    }
}
